# Connector architecture

How the framework works, how each source maps its vendor's model onto IronRAG,
and the invariants that must hold. One document because there is one
distribution: a rule that lived in one connector's notes and not another's is
how the four drifted apart.

---

## Framework

### Where the framework sits

```
┌──────────────────┐  vendor REST / webhook  ┌─────────────────────────────┐
│  vendor source   │ ────────────────────────│  your-connector             │
│  (wiki / CMS /   │                         │   ┌──────────────────────┐  │
│   filesystem)    │                         │   │   YourSourceAdapter  │  │
└──────────────────┘                         │   └──────────────────────┘  │
                                             │              │              │
                                             │   ironrag_connector         │
                                             │   ┌──────────────────────┐  │
                                             │   │ routing / state /    │  │
                                             │   │ policy / sync /      │  │
                                             │   │ orchestrator / http  │  │
                                             │   └──────────┬───────────┘  │
                                             └──────────────┼──────────────┘
                                                            │
                                                            │ POST /v1/content/libraries/{libraryId}/documents
                                                            │ POST /v1/content/documents/{documentId}/revisions
                                                            │ DELETE /v1/content/documents/{documentId}
                                                            ▼
                                                ┌────────────────────────┐
                                                │        IronRAG         │
                                                └────────────────────────┘
```

The adapter is the only piece of code that knows the vendor API.
Everything else is framework.

### Lifecycle of one item

```
iter_items ──► SourceItemRef (id, kind, external_key, token, facts, enforcement)
                       │
                       ▼
                  Router.resolve
                       │
          ┌────────────┼──────────────────┐
          │            │                  │
       matched    no rule/default   routing/catalog error
          │            │                  │
          ▼            ▼                  ▼
  StateStore.get   POLICY: unrouted   preserve target+cursor
                   REQUIRED: exact         │
                   target absence          ▼
                                    OrchestrationOutcome(unrouted)
            │
   ┌────────┴────────┐
   │                 │
change_token       change_token
unchanged          advanced (or no row)
   │                 │
   ▼                 ▼
ironrag.find    adapter.fetch(ref) ─► SourceFetchResult
   │                                     │
existing?                  ┌─────────────┼──────────────┐
   │                       │             │              │
   ▼                  Materialized    Deferred        Delete
noop_unchanged        (POLICY|REQUIRED) preserve     (POLICY|REQUIRED)
                           │          target+cursor      │
                           ▼                             ▼
                  Orchestrator.push_item             delete/retain
                           │
                  ┌────────┼────────┐
                  │        │        │
            not present  present  on 409 duplicate
                  │        │        │
                  ▼        ▼        ▼
             on_new=... on_changed=... on_duplicate_content=...
                  │        │        │
                  ▼        ▼        ▼
             created  replaced  skipped_duplicate_content
                           │
                           ▼
                  StateStore.upsert
```

A valid routing snapshot with no matching rule and no default raises the typed
`UnroutedError`. A `POLICY` ref remains `unrouted` without mutation. A
`REQUIRED` ref instead establishes target absence by finding and deleting every
exact-key copy, even under `on_missing=ignore`; the adapter is not fetched.
Other `RoutingError` failures, such as an unresolved catalog target, are
uncertainty rather than an intentional routing decision, so they retain content
and cursor state. A rejected hot reload also keeps the last valid routing
snapshot active.

`SourceFetchMaterialized` carries complete bytes and metadata plus `POLICY` or
`REQUIRED` enforcement. `POLICY` follows `on_changed`. `REQUIRED` accepts a
visibility intent for every existing exact-key copy before submitting the current
replacement, regardless of `on_changed=skip`. Historical copies are retired by
public denial, not physical deletion; foreign pending winners may finish but
remain invisible. No payload is submitted to retired routes. `on_new` remains
authoritative for genuinely absent targets.

`SourceFetchDelete(POLICY)` follows `on_missing`; `SourceFetchDelete(REQUIRED)`
establishes physical absence even under `ignore`. Its typed exclusion is counted
independently in the sync report. This separate deletion contract is unchanged.
`SourceFetchDeferred` performs no visibility write and advances no applied token.

Synchronization requires `contentVisibilityContract: "visibility-intent/1"`
before source enumeration. Missing or mismatched capability fails without
negotiating an older protocol. REQUIRED refs are completely refetched even when
the opaque source token equals the applied token. Before recording success, the
connector proves matching payload READY and current revision/item/intent lineage,
then revalidates every historical denial and discovers the exact-copy closure.
An old successful receipt alone is not proof of the current public head.

SQLite persists the source identity (adapter/kind/item/external key), desired
target, complete replacement descriptor, nullable materialization token and
observation episode before writes. A→B→A observations produce distinct episodes;
retransmission reuses the current one. Unknown HTTP receipt replay keeps the
exact visibility key and original floor. Accepted receipts are not job receipts
or continued-currentness claims. Terminal payload retries change only their
payload key, retaining the intent/floor; supersession alone never creates a new
intent. Server fingerprints remain opaque evidence, not client-computed digests.

Denial reconciliation visits every discovered copy independently. A superseded
or unknown first copy does not prevent denial of later copies. Any unresolved
copy withholds payload submission and applied-token success; replay does not
mint a new intent for the superseded copy without another source observation.
The local applied-token transaction compares the observation and the exact
persisted attempt phases, keys and receipts for current and retired copies,
plus explicit expected copy membership, including REQUIRED creation with no
historical attempts. This SQLite CAS is not a distributed
transaction with backend head changes: server acceptance order remains the
authority, and the connector does not establish upstream chronological freshness.

Ordinary POLICY replacement uses the same durable materialization observation
and per-copy attempt owner, with an optional intent request/receipt. It does not
submit a visibility denial. Before HTTP submission, SQLite stores the complete
client payload identity (target, operation, checksum, byte size, filename, MIME,
title, hint, language/URI, intent and append base when present) and an opaque
payload key. This is not a clone of the backend request fingerprint. Changed
complete identity creates a new episode/key; an unknown send or process reopen
replays the exact identity/key. Only a proven terminal payload failure rolls the
key within the same episode. A caller's `idempotency_key` is a logical namespace
included in the identity, not the raw transport key: each durable episode
qualifies it with its own opaque key. Typed idempotency conflicts surface as
errors, never an indefinite busy deferral. Visibility requests, receipts and keys
are persisted in SQLite.

For REQUIRED, an accepted intent retains the namespace of its first bound payload
identity. Refetching the same descriptor and token with another caller namespace
replays that binding; it neither rewrites the identity nor captures a new floor.
Before handling any polled terminal disposition, including exception-carried FAILED
statuses, operation identity must match the accepted payload receipt. A foreign
terminal acknowledgement cannot discard that receipt or roll its transport key.

The reaper runs only after a complete census: for every primary `kind` the
adapter declares, the framework walks every IronRAG list page under the
adapter's external-key prefix and deletes any active document whose
`(kind, item_id)` was not seen this sweep. `iter_items` finishing normally is
the adapter's proof that every live primary item was yielded. An adapter that
loses that proof part-way (an unreadable repository, an unparseable listing
record) keeps yielding what it can and then raises `IncompleteCensusError`;
any other exception is a failed enumeration. In both cases the enumerated
items are still pushed, `SyncReport.census_complete` stays `false`, the
sweep counts one error and nothing is reaped, because absence from an
incomplete census is not evidence of deletion. Items an operator excluded by
configuration are outside the census scope, not missing from it. Every
adapter must declare `missing_enforcement(kind)` explicitly. `POLICY` follows
the kind's `on_missing` setting; `REQUIRED` deletes after a complete census
even when that setting is `ignore`.

Dependent kinds (a page's attachments and inline images) are never enumerated;
they exist only as the `dependents` tuple of a parent fetch. Their lifecycle is
durable membership, not the sweep's `seen` set. A complete source fetch is the
proof of its actual dependent set: the orchestrator claims each dependent in
`dependent_membership` before pushing it, records the parent's census (its
`change_token` and `dependents_token`) once every dependent is applied, and in
the same transaction releases the claims that fetch omitted. Released copies
are deleted only by the completeness-gated dependent reaper, subject to the
dependent kind's `on_missing`. No remaining claims is not sufficient orphan
proof while an enumerated legacy owning parent has no census; a shared image
may still belong to it. A copy another parent claims stays. A dependent the
server proves is unsupported
(`content_unsupported`, whether from a failed operation or a synchronous 400
admission problem) is terminally accounted: it is recorded as an
enumerated member but not materialized, its claim and any existing server copy
are kept, and the census settles as usual. A deferred dependent push, or one
that fails without a server-proven content rejection, records no census, so
nothing is released. A parent that vanishes (a fetch delete, a webhook delete,
a required no-route, absence from a complete census) releases its whole set; a
parent the policy retains keeps it. Adapters bound dependent drift with
`SourceItemRef.dependents_token`, derived from a per-sweep index that is
independent of the parent's own token: when a parent's primary token is
unchanged but its census is absent or recorded at other tokens, the
orchestrator fetches it anyway, confirms the primary copy without rewriting it,
and proves the dependent set. A ref without a `dependents_token` is never
proven and is refetched every sweep.

Dependent settlement is independent of primary success after a validated,
identity-bound terminal POLICY primary operation failure. The same dependent
phase applies the actual tuple, never an invented empty census. The typed
`OrchestrationFailedError` retains the original primary exception, completed
dependent outcomes, and any dependent-phase exception. Sync counts the primary
error and actual dependent counters (plus a dependent error if present);
webhooks still receive an exception. The failed primary cursor stays unchanged
and historical operations stay failed on later retries. A failed/deferred
dependent writes no census. Unvalidated failures, incomplete fetches, and
REQUIRED materializations do not qualify for independent settlement. Neither
ownership nor the dependent reaper's completeness gate is relaxed.

The reaper's dependent phase runs after the primary phase and only when
`SyncReport.dependent_census_complete` is true: the census was complete and
every enumerated primary that owns copies holds a parent census. A primary
owns copies once this connector has pushed it (a cursor) or claimed a member
for it; that is exactly the state in which unclaimed copies can exist —
copies from a fetch that died before proving its set. An enumerated item this
connector never pushed and never claimed for
(unrouted, or one whose only copy is already deleted) cannot own an unclaimed
dependent, so it does not withhold the phase; withholding on it would leave
real orphans unreapable for as long as the source keeps listing it. One
unproven owning parent does withhold the phase for the whole sweep
(`sync.dependent_census_incomplete`, which names the blocking parents); the
proving refetch closes that gap on the following sweeps.

Webhook-only connectors with dependent kinds run a request-driven background
worker. Every authenticated, parsed webhook delivery requests the canonical
`SyncManager.run_once` sweep, even when its handler raises after a dependent
settlement. The response does not await synchronization. Requests coalesce;
the worker waits for an active manual sweep and uses the existing completeness
gate before deleting released copies. Incoming deliveries during a sweep request
one follow-up; sweep mutations never request more work themselves. There is no
periodic poller or automatic retry loop for unproven owners in webhook mode.

A parent absent from a complete census is forgotten whether or not it was ever
proven: claims are written before their dependent's push, so a parent can hold
claims with no census row, and skipping it would leave the copies it claimed
unreapable. Withdrawing a library the target no longer has
(`StateStore.delete_by_library`) drops the claims in it and the censuses that
proved them together, so a narrowed membership is never mistaken for a
parent's own verdict.

Within one connector process, full sweeps are single-flight. Manual
`/sync/run`, startup, and periodic triggers all share the same
`SyncManager.run_once()` guard, so they cannot concurrently mutate the same
cursor database or push duplicate operations to the same target. A concurrent
manual trigger receives HTTP 409; periodic and startup triggers log a skipped
event. If a manual request is cancelled by the caller, the framework cancels
any in-flight item tasks and emits `sync.cancelled` before releasing the guard.
Each source ref is also bounded by `SYNC_ITEM_TIMEOUT_SECONDS`, including
adapter fetch, primary push, and dependents. A timed-out item is cancelled,
counted in `item_timeouts`, and the sweep continues; `sync.item.start` and
`sync.item_timeout` identify the affected source ref. A failed operation the
server proves is a content rejection
(`failureDisposition=content_unsupported`) is counted in `skipped_unsupported`
(`sync.item_content_unsupported`), as is an HTTP 400 admission rejection that
carries the same explicit extension; every other failure -- `retry_exhausted`,
`operational_terminal`, a null or unknown disposition (an older or newer
server), and every other HTTP problem (401/403/404/409/429/5xx, even when the
extension is also present) -- stays in `errors`, and `failureCode` is audit
evidence, never the decision. Once a ref
has timed out, later sweeps bound it by the shorter
`SYNC_ITEM_RETRY_TIMEOUT_SECONDS`, so a stuck ref cannot burn the full budget
on every sweep. The full budget returns on the first attempt and on every
`FULL_BUDGET_ESCALATION_ATTEMPTS`-th consecutive attempt, so a legitimately
slow ref can still ingest; any non-timeout completion clears the count.

Push, listed-document reap and exact-absence confirmation for the same
`(kind, item_id)` also share one process-local identity lock. Reaping compares
the pre-sweep cursor and location evidence before a mutation, so a webhook or
item push that changes the identity after listing makes the reap defer instead
of deleting fresh state. This is a single-process guarantee. Supported
deployments run one replica with `Recreate`, and the server pidfile rejects a
second process using the same state; the framework does not provide a
distributed lock for multi-replica operation.

The shared HTTP client applies `REQUEST_TIMEOUT_SECONDS` to each IronRAG
request, including create, revision and delete admission and each operation
status read. There is no separate or derived mutation-admission timeout. An
HTTP request timeout follows the normal item or webhook error path; it is not
converted into a deferred outcome, and the new source token is not recorded as
applied. A timed-out admission may still have reached IronRAG, so the timeout
does not prove that no mutation occurred; the next sweep reconciles the exact
remote state.

Accepted revisions and deletes are polled through
`GET /v1/ops/operations/{id}`. `OPERATION_POLL_INTERVAL_SECONDS` controls the
poll cadence and `OPERATION_POLL_BUDGET_SECONDS` bounds the whole poll. A
revision that does not become terminal within that budget is deferred to the
next sweep without applying the new token. Its dependents wait, and a deferred
dependent restores the primary cursor to its prior version and records no
parent census. A delete poll
timeout propagates as an error and leaves its state available for exact-key
reconciliation on a later sweep. Sync reports count structurally deferred
revision outcomes in `deferred`; they do not classify generic request timeouts
that way.

The orphan reaper also has its own bounded IronRAG list budget.
`REAPER_LIST_TIMEOUT_SECONDS` wraps each prefix-list request made after source
enumeration. If the backend listing endpoint is slow, the reaper skips that
library/prefix for the sweep and releases the single-flight sync lock instead
of holding `/sync/run` until the generic HTTP timeout expires.

The comparison also includes the routed library, but a cursor is not a
document-ownership record. `cursor` stores the successfully applied source
token and current route; `document_location` is the sole durable registry of
every verified `(source item, library, document)` copy. Before enumeration,
each sweep snapshots a per-kind scan scope that unions every cursor route with
every verified location library, plus the corresponding rows as compare-and-set
evidence. Current routing targets are added during reaping. A cursor-only row
can therefore nominate a library for exact discovery without claiming a
document in it.

If routing moves a still-existing item, the new copy is registered without
forgetting the old location. The reaper can inspect the old library and either
delete that copy or retain it under `on_missing=ignore`. A failed delete leaves
its location registered for retry. An exact empty scan clears only the target
location and a cursor that still points to that library; a cursor already moved
to a new library is preserved. If either row changed since the pre-sweep
snapshot, the cleanup defers. If an enumerated item establishes no successful
route or push target, existing copies are retained for transient fetch and
routing failures; the typed `REQUIRED` intentional no-route case above is the
exception.

The cursor's `current_library_id` is scan scope only, not proof of document
ownership. Only exact-key discovery or a validated create/revision can populate
`document_location`.

Before a discovered document ID can authorize a no-op, revision or delete, the
framework validates the requested library and exact canonical external key and
records that verified location. Required materialization and deletion use the
same scope across current routing targets, the pre-sweep cursor/location union,
the current cursor and durable locations. A source ref must also equal
`adapter.external_key(kind, item_id)` and round-trip through
`parse_external_key` before routing, fetching or any IronRAG mutation; malformed
identity therefore cannot redirect a delete.

### Why a Protocol instead of inheritance

`SourceAdapter` is a `typing.Protocol` so adapters can be plain classes
without imports from the framework's class hierarchy. This keeps the
framework dependency surface flat (you only import the data shapes you
return) and makes adapters trivially mockable in tests.

### Why SQLite for state

The diff stage compares each `change_token` against the value stored at the
last successful push. JSON works until two writers touch the file at once
(manual `/sync/run` overlapping a periodic sweep, two workers in docker
compose, …). SQLite gives atomic committed transitions in one daemon-free file.
The store runs in WAL mode with `synchronous=NORMAL`; it does not claim an fsync
for every write. `cursor` is keyed by `(kind, item_id)` and stores
`change_token`, canonical `external_key`, `current_library_id` and
`last_pushed_at`. `document_location` is keyed by
`(kind, item_id, ironrag_library_id)` and stores the canonical external key,
IronRAG document id and observation time. This permits one source item to have
known copies in several libraries during a route move without overloading the
source cursor. `parent_census` is keyed by the parent `(kind, item_id)` and
stores the `change_token` and `dependents_token` at which its dependent set was
last proven. `dependent_membership` is keyed by
`(parent_kind, parent_item_id, kind, item_id, ironrag_library_id)`: one row per
claim a parent holds on a dependent copy in a library. Both survive restarts,
so a proven parent is not refetched after a restart and a stale dependent copy
is reaped without one.

### Why per-kind policy

Different items derived from the same source have different update
shapes. Pages: create + replace + delete. Images: create + replace +
*ignore* on missing (content-addressed; a digest no live page references
right now may return). Drafts: skip new + skip changed + delete on
missing (only push when published).

For a dependent kind, `on_missing` governs a copy no proven parent claims:
one released by a parent's complete fetch, one whose parent vanished, or one
listed by the dependent phase without a claim. A copy another parent still
claims is not missing and is never deleted by that policy; `ignore` retains
the unclaimed copy and its membership stays released.

Making policy a per-`kind` map keeps the framework's identity model
honest: every push decision is `(kind, item_id) → policy`.

### Failure modes

| Symptom | Framework behaviour |
|---------|---------------------|
| `iter_items` raises `IncompleteCensusError` after yielding what it could | Enumerated items are pushed, `sync.census_incomplete` names the failed scopes, `census_complete=false`, reaper is **not** run. Next sweep retries. |
| `iter_items` raises anything else | Same outcome via `sync.enumeration_error`: pushes already started are awaited, reaper is **not** run (would falsely delete unseen items). |
| `fetch` returns `SourceFetchDeferred` | No IronRAG call or cursor update; the existing target remains eligible for a later stable sweep. A parent's previously proven dependent set is kept as is. |
| A dependent of a materialized parent is deferred | The primary cursor is restored, no parent census is recorded and nothing is released; the claim the push made stays, so the copy is not reaped. |
| A parent's complete fetch omits a dependent it claimed before | The claim is released with the census; physical deletion waits for the completeness-gated dependent reaper and follows the dependent kind's `on_missing`. An unproven legacy owner withholds cleanup even without a claim. |
| An enumerated parent has no parent census at the end of a complete sweep | `dependent_census_complete=false`, `sync.dependent_census_incomplete`; primary kinds are reaped, dependent kinds are not. The orchestrator refetches the parent on the next sweep to prove it. |
| `fetch` returns `SourceFetchDelete(POLICY)` | The kind's `on_missing` policy decides whether to delete or retain the target. |
| `fetch` returns `SourceFetchDelete(REQUIRED)` | Every known or exactly discovered target copy must be absent even when `on_missing=ignore`; locations and the materialized cursor are removed only as each absence is established, and a cacheable decision is recorded only after all deletions succeed. A visibility-forced REQUIRED decision also increments `SyncReport.skipped_limited_visibility`, independently of `reaped`/`skipped_empty`: the same decision also lands in one of those counters, but only this one says the visibility gate is what excluded the entity, and a still-limited entity keeps producing the decision on every sweep without another deletion. |
| A `SourceFetchMaterialized(REQUIRED)` current-route apply fails or is deferred | Retain the target and independently accepted denial, leave the source token unapplied, and replay the durable request. A receipt-free transport failure remains unknown acceptance, never fabricated protection. Do not delete a foreign payload winner. |
| Valid routing config has no matching rule or default | `UnroutedError` is intentional. `POLICY` preserves content as `unrouted`; `REQUIRED` establishes exact-key absence even under `on_missing=ignore`. |
| Route/catalog resolution fails | Existing content and cursor state are preserved; the error is not treated as an intentional no-route decision. |
| IronRAG 5xx | Raised as `IronRagError`; orchestrator surfaces in `sync.errors` and continues with the next item. |
| IronRAG polls a failed operation carrying `failureDisposition=content_unsupported` | Counted in `skipped_unsupported` (`sync.item_content_unsupported`), not `errors`; the item stays in the sweep's keep-set, so it is not reaped, but its bytes cannot become ingestible. Every other disposition (`retry_exhausted`, `operational_terminal`), a null or unknown disposition (an older or newer server) and any HTTP problem (401/403/404/409/429/5xx) stays in `errors`; `failureCode` is audit evidence, never the skip decision. |
| IronRAG rejects an upload admission with HTTP 400 carrying `failureDisposition=content_unsupported` | The synchronous carrier of the same proof, counted in `skipped_unsupported` (`sync.item_content_unsupported`), not `errors`. The extension on any other status (401/403/404/409/429/5xx), a 400 without it, or an unknown value stays in `errors`; a 409 `duplicate_content` keeps its own typed error. The server is not required to emit the extension yet: without it the connector fails closed. |
| A dependent the server proves unsupported | Terminally accounted in its parent's census: claimed and kept (never reaped because of the terminal outcome), recorded as an enumerated member but not materialized, and the census settles normally so `dependent_census_complete` can become true. Any other dependent failure (including `retry_exhausted`) records no census and withholds the dependent phase. |
| One of several copy deletions fails | Successfully deleted locations stay removed, failed locations and any still-relevant cursor remain retryable; no authoritative-absence cache is written. |
| IronRAG 409 duplicate content | `on_duplicate_content=skip` records the cursor but no conflicting document ID or location; `fail` raises. The skipped source remains eligible for a later exact lookup/create attempt. |
| Vendor returns retry-able status (429 / 5xx) | The adapter's transport layer is responsible for retry-with-backoff. The framework does not retry on the vendor side; see `bookstack/src/bookstack_connector/bookstack.py:97` for a reference implementation. |
| Two processes start with the same `STATE_DB_PATH` | The pidfile lock fails the second one loud. SQLite would otherwise handle the concurrency, but two parallel sweeps would burn vendor quota. |

### Webhook handlers

The framework does not assume a vendor webhook format. Adapters that
*do* receive webhooks register one or more `WebhookHandler`s; the
framework mounts each at `/webhook/{name}`, does bearer-token auth,
parses JSON, and hands the dict to the handler. The handler is expected
to translate the payload into one or more `SourceItemRef`s and call
`Orchestrator.push_ref` on each — same code path as the sync loop.

This keeps webhooks and periodic sync coherent: a `page_update` webhook
runs through identical routing + policy + state machinery as the same
page surfacing on the next sweep.

### What the framework deliberately does NOT do

- It does not assume any vendor's authentication, retry, or pagination
  shape. That lives in the adapter's transport layer.
- It does not own a vendor-specific data model. Books, shelves,
  databases, spaces, notebooks — all of those are facts the adapter
  emits via `routing_facts` and the routing config can match on.
- It does not retry IronRAG mutation admission in process. A later sweep uses
  a payload-derived key for the same revision; every delete attempt uses a
  fresh UUID and relies on exact-key reconciliation to determine what remains.
- It does not hold an in-process queue. The sync loop is the queue:
  each periodic pass is a fresh reconciliation sweep.


---

## BookStack

### Position in the stack

```
┌─────────────────┐  page_create/update/delete   ┌──────────────────────┐
│   BookStack     │ ───────────────────────────▶ │  bookstack-connector │
│   (wiki)        │   webhook (HMAC or Bearer)   │   (this service)     │
└────────┬────────┘                              └──────────┬───────────┘
         │                                                  │
         │ /api/pages/{id}/export/markdown                  │ POST /v1/content/libraries/{id}/documents
         │ /api/pages/{id}                                  │ POST /v1/content/documents/{id}/revisions
         │                                                  │ DELETE /v1/content/documents/{id}
         │                                                  │ GET /v1/ops/operations/{id}
         ▼                                                  ▼
┌─────────────────┐                              ┌──────────────────────┐
│   BookStack     │                              │       IronRAG        │
│   REST API      │                              │   (canonical RAG)    │
└─────────────────┘                              └──────────────────────┘
```

The connector is the **only** component that holds vendor knowledge. IronRAG
keeps using its existing `content_document.external_key` as the canonical
identity surface; everything BookStack-flavoured (token format, retry headers,
event names) is contained here.

### Event lifecycle

1. BookStack fires a webhook on page create / update / delete.
2. Connector authorises the request (HMAC SHA-256 over body **or** Bearer token).
3. `webhook.parse_event` filters for `page_create | page_update | page_delete`;
   non-page events return `202 ignored`.
4. The handler maps the event to a `SourceItemRef` and hands it to the
   framework orchestrator:
   - For create/update: `Orchestrator.push_ref` fetches the page export
     (markdown by default) via BookStack REST, looks up an existing IronRAG
     document by `external_key = bookstack:page:<id>`, then either creates the
     document or creates a new revision.
   - For delete: `Orchestrator.delete_by_ref` looks up the document, calls
     `DELETE /v1/content/documents/{id}`.
5. Revisions and deletes are asynchronous on the IronRAG side: the API hands
   back an operation id which the framework polls to a terminal state via
   `GET /v1/ops/operations/{id}`. Chunk reuse + the image-checksum gate kick
   in for revisions (no full re-embed for unchanged content).

Document creation carries no idempotency key. IronRAG can answer with a typed
duplicate-content 409 naming an existing exact-key or byte-identical document.
Under `on_duplicate_content=skip`, the connector records the source cursor but
does not adopt that document ID or write a location; `fail` raises instead.
Revisions carry a payload-derived content-hash key, so the same revision request
can be submitted again safely. Each delete admission, including a required
cleanup, receives a fresh UUID-based key. Delete retries converge through
exact-key rediscovery and absence confirmation, not a reused content hash.

### Failure modes and behaviour

| Event from BookStack | Outcome on the IronRAG side |
|----------------------|-----------------------------|
| `page_update` for an unknown page (404 from `/api/pages/{id}/export`) | Returns `SourceFetchDelete(POLICY)`; `on_missing` decides whether exact-key copies are deleted or retained. |
| `page_delete` for a page never ingested | Target absence is confirmed; no IronRAG mutation is needed. |
| `page_delete` for an ingested page with attachments and images | The page copy is deleted and its dependent membership released; attachments and images await the completeness-gated dependent reaper, which follows their kind's `on_missing`. |
| `/api/attachments` (the per-sweep attachment index) fails | The enumeration fails loud: pages cannot be yielded with an unknown dependent set; nothing is pushed or reaped this sweep. |
| A page's attachment list, an attachment payload or an inline image download fails during fetch | The page is deferred; its previously proven dependent set is kept. A foreign host answering for an external attachment, and a 2xx that is not an image, are read failures too: only BookStack's own 404 for an upload excludes the image from the set and proves that copy gone. |
| BookStack returns 429 / 5xx | up to `BOOKSTACK_RETRY_MAX` retries with `Retry-After` honoured |
| IronRAG returns 4xx | propagated as a typed `IronRagProblemError` (RFC 9457 problem details); FastAPI returns 502 to BookStack so it can retry |
| Webhook missing/invalid auth | 401 / 403; BookStack admin sees the failure and rotates the secret |

### Mapping rules

* **External key** is the only identity surface used:
  `bookstack:page:<page_id>`. A rename or slug change keeps the same key;
  IronRAG sees a new revision, not a delete + create.
* **Dependents token** of a page is the sha256 of its attachments' `id` and
  `updated_at` from the instance-wide `/api/attachments` index, read once per
  sweep. A re-uploaded, added or removed attachment moves it and the page is
  refetched to re-prove its set even when the page itself is unchanged. Inline
  images are bounded by the page's own `updated_at`: the markdown referencing
  them changes with the page.
* **Books / chapters** are not ingested. Their identifiers/slugs are emitted
  as routing facts when BookStack exposes them.
* **Routing facts** are summary-first. During a full sweep the adapter emits
  `page_id`, `page_slug`, `name`, `book_id`, `book`, `chapter_id`, `chapter`,
  `shelf_ids`, `shelf`, and `tag`. It builds full page context and refreshes
  the shelf reverse-index only when `routing.yaml` rules require facts missing
  from the `/api/pages` summary, such as `shelf`, `shelf_ids`, or `tag`.
* **Format**: `BOOKSTACK_PAGE_FORMAT` defaults to `markdown` because IronRAG's
  Markdown adapter is the cleanest path; `html`, `plaintext`, and `pdf` are
  also accepted (the orchestrator forwards the matching MIME type to IronRAG).

### What the connector deliberately does NOT do

* No BookStack-specific sync state outside the framework cursor. Polling,
  webhook handling, policies, and orphan reaping are owned by the shared
  connector framework.
* No vendor-specific logic in IronRAG itself — the IronRAG repo only sees
  documents and revisions tagged with an external key.
* No persistence of its own. The mapping `bookstack:page:<id> → IronRAG
  document_id` is owned by IronRAG (`content_document.external_key`).
* No HMAC verification of outbound IronRAG signatures (the connector is a
  sender, not a receiver, of IronRAG webhook events; if you also want to
  receive `revision.ready` from IronRAG and forward elsewhere, that lives in
  a separate subscriber service).

### Operational notes

* Sized for low write throughput (typical wiki traffic: dozens of edits per
  hour). One uvicorn worker handles thousands of webhooks per hour easily; a
  reverse-proxy ahead of the connector is mandatory for TLS but optional for
  load distribution.
* Logs are structured JSON via structlog; key fields per event include
  `page_id`, `event_kind`, `external_key`, `action`, and
  `ironrag_document_id`.
* Health: `GET /health` returns `{"status": "ok"}`.
* Routing targets are canonical `<workspace-slug>/<library-slug>` catalog
  references. The framework resolves them at startup and on a successful
  `routing.yaml` hot reload, so internal ID changes do not leak into operator
  configuration. One connector instance may route pages and their dependents
  across multiple IronRAG libraries and workspaces.


---

## Git repositories

### Overview

```
                          repos.yaml                  routing.yaml
                              │                            │
                              ▼                            ▼
git remote / local tree ──► GitReposAdapter ──► ironrag_connector framework ──► IronRAG
       (git CLI)                                  (routing / state / sync)        HTTP
```

The adapter is the only piece of code that touches git. Everything
else — the cursor, the per-kind policy, the orphan reaper, the
HTTP server — is the connector framework.

### Lifecycle of one file

```
ensure_repo(spec)              ── clone bare or git fetch --prune for every spec
   │                              (branches kept in refs/heads/*, mirrored on fetch)
   │
   ▼
list_tree(spec, branch)        ── git ls-tree -r -l -z <ref>
   │
   ▼
yield SourceItemRef            ── change_token = blob sha; item_id = <repo>\0<branch>\0<path>
   │
   ▼
Router.resolve                 ── facts include repo, branch, path, dir, ext, top_dir, facts.*
   │
   ▼
StateStore.get                 ── cursor row keyed on (kind, item_id)
   │
 ┌─┴─────────────┐
 │               │
unchanged   change_token advanced (or new row)
 │               │
 ▼               ▼
noop      fetch(ref) → SourceFetchMaterialized(SourceItem(git cat-file blob <sha>))
                 │
                 ▼
        Orchestrator.push_item
                 │
        ┌────────┼────────┐
        │        │        │
   not present  present  on 409 duplicate
        │        │        │
        ▼        ▼        ▼
   on_new    on_changed   on_duplicate_content
        │        │        │
        ▼        ▼        ▼
   created   replaced   skipped_duplicate_content
                 │
                 ▼
        StateStore.upsert (cursor; verified location only for an owned doc)
```

After a complete census the reaper lists IronRAG documents under the
`gitrepos:file:` prefix and deletes anything not seen during the sweep,
subject to the `file` kind's `on_missing` policy in `routing.yaml`.

The census unit is one `(repo, branch)` scope. A scope whose clone, fetch or
tree listing fails is skipped so the other scopes still ship their updates,
and `iter_items` then raises `IncompleteCensusError` naming every failed
scope, so that sweep reaps nothing. `git ls-tree` output is parsed against an
exact grammar; a record that does not fit fails the scope instead of being
dropped.

Operator admission rules — `include`/`exclude` globs, `max_file_bytes`
(global and per repo), the repo and branch lists — define the census scope.
The census is complete over that scope: a previously indexed file that leaves
it is reaped or retained by `on_missing`, exactly like a file deleted from the
branch. Only content inside the scope that cannot be represented as text (a
binary blob) is a `REQUIRED` delete, decided in `fetch()`. A path that is not
UTF-8 cannot be minted into an external key and so can never have a document;
it is outside the representable scope and logged.

### External-key layout

```
gitrepos:file:<repo>:<branch>:<path>
```

Repo, branch, and path are URL-encoded (RFC 3986 unreserved). Reasons:

1. Colons inside branch names (rare but allowed by git for refs) or
   inside paths (forbidden by git on the working tree but
   theoretically possible) would otherwise break parsing.
2. `parse_external_key` is the round-trip the reaper needs to map an
   IronRAG document back to a `(kind, item_id)` for cursor cleanup.

### Change-token choice: blob sha

git exposes two natural identities for "this file at this revision":

* **commit sha** — advances on every commit that touches the file.
* **blob sha** — advances *only* when content actually changes.

A connector that re-shipped a file on every commit would burn the
embedding / graph pipeline whenever an unrelated commit touched the
tree (a Makefile bump, a comment-only rebase). We pick blob sha
deliberately: only real content changes reach IronRAG.

### Why git CLI instead of pygit2 / GitPython

* No native library means a smaller image (`git` is ~50 MB,
  `pygit2`'s libgit2 build chain is ~150 MB on top of that).
* git's own SSH / credential-helper / submodule handling is the
  authoritative implementation; libgit2 lags on edge cases.
* Each command is a clean subprocess — no in-process state to
  pollute, no thread-safety questions for the async sweep.

The trade-off is one fork+exec per `ls-tree` and per `cat-file`.
For reasonable repo sizes (≲ 50k files) this is negligible; for
multi-million-file monorepos it would warrant batching.

### Failure modes

| Symptom                                                  | Behavior                                                                                                                |
|----------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------|
| `git clone` fails (auth, network)                        | `gitrepos.repo.ensure_failed` is logged, every branch of that repo is a failed scope, the other repos are still enumerated and pushed, and the sweep ends with `IncompleteCensusError`: nothing is reaped. |
| `git fetch` fails on an existing clone                   | Same as clone failure.                                                                                                  |
| Branch absent from a remote (typo, deleted upstream)     | `gitrepos.branch.list_failed` is logged; that branch is a failed scope and the sweep reaps nothing. Remove the branch from `repos.yaml` to make its files orphans. |
| Malformed `git ls-tree` record                           | `GitTreeParseError` fails that branch's listing: same as an absent branch.                                              |
| File larger than `max_file_bytes`                        | `gitrepos.file.skipped_too_large` is logged; the file is outside the admitted scope, so its prior copy follows `on_missing`. |
| File rejected by `include`/`exclude`                     | Outside the admitted scope; its prior copy follows `on_missing`.                                                        |
| Path with invalid UTF-8                                  | `gitrepos.tree.path_not_utf8` is logged; no external key can exist for it, so there is nothing to reap.                |
| Vendor `git@host:` URL with custom port                  | Set up via an ssh config mounted at `/var/lib/ironrag-connector/.ssh/config` (the connector user's home); the connector itself has no custom-port flag. |
| `repos.yaml` changed while the process is running        | The file is reloaded before the next sweep; include/exclude, repo, branch, and fact changes use the new rules.          |
| Repo removed from `repos.yaml` after some files shipped  | All files under that repo become orphans on the next clean sweep and are reaped per the `file` `on_missing` policy.     |

### Operational notes

- The clone cache lives under `GITREPOS_CLONE_ROOT` (default
  `./repo-cache`). Mount a docker volume so clones survive restarts.
- A remote repo is a bare clone whose branches live in `refs/heads/*`;
  `ensure_repo` resolves them there and fetches the mirror refspec
  `+refs/heads/*:refs/heads/*`, because a bare repository has no
  remote-tracking refspec of its own. A branch absent from the mirror is a
  missing branch: `head_sha` returns `None` and its tree scope fails.
- For remote repos the cache uses `--filter=blob:none` partial clones:
  trees come down with the initial fetch, blob bodies stream on
  demand at `git cat-file`. This keeps the cache size proportional
  to *changed* files, not to the full repository history.
- For local-path repos the connector reads the working tree's
  current branch tips. If you want the connector to follow a
  rebase, point it at a checkout you push your branches into; do
  not point it at a developer's daily working copy where uncommitted
  changes would be ignored.
- Sweep concurrency (`SYNC_CONCURRENCY`) is the bound for parallel
  IronRAG uploads, not for parallel git operations. Git commands
  per repository are serialized via a per-repo asyncio lock to
  avoid `git fetch` racing itself.


---

## YouTrack

This connector emits two independent primary kinds: `article` (Knowledge Base
wiki pages) and `issue` (tasks). Neither is the other's parent: an issue is
never enumerated through an article fetch or vice versa, so both are
enumerated directly by `iter_items` and both are reaped independently by
ConnectorTemplate. `KINDS`/`primary_kinds` on `YouTrackAdapter` list both.

Comments, attachments and links are part of their canonical parent document.
They are not separate primary documents and never get a second external key.
An added or edited safe child changes the parent's composite token and parent
revision. Deletion or restricted access to a previously indexed child selects
required parent deletion first; a later stable sweep may create the remaining
safe projection as a new document.

Comment sync is decided per kind by the routing policy table:
`policies.issue.include_comments` and `policies.article.include_comments` in
`routing.yaml`, both default `true` and independent, with no environment
counterpart. The framework builds the adapter through the plugin factory after
routing is loaded and hands it the live `PolicyOverrides` object -- the same
table the routing reloader replaces in place -- so the adapter resolves
`for_kind(kind).include_comments` at enumeration and fetch time and follows a
hot reload without a restart. A `policies` entry for a kind the adapter never
emits is rejected at start-up and on reload (a rejected reload keeps the
previous snapshot), so a misspelt kind cannot leave comments silently enabled.

For a disabled kind the adapter never requests that kind's comment endpoint,
projects no comment section, drops every attachment descriptor whose proven
owner is a comment (an unprovable owner still fails closed as unknown
visibility), and for issues hashes `commentsCount` as `null` and skips the
completeness gate. Flipping the value changes the hashed projection: the next
sweep rematerializes every issue (`commentsCount` is hashed as `null` versus an
integer) and every article that had comments or comment-owned attachments; an
article without either keeps its token because its output is unchanged. Should
the table change between enumeration and fetch of one item (a reload runs at
sweep start, so only a webhook-capable source can hit this), comments the ref
proved indexable are missing from the live projection and the item takes the
existing tightened-readability path: `SourceFetchDelete(REQUIRED)`, recreated
by the next sweep under the new token.

### Stable snapshots

```text
YouTrack /api/articles        YouTrack /api/issues
                              (sort by: created asc, {issue id} asc)
        │  two complete,                 │  two complete passes;
        │  identical paginated           │  only the ID set must match,
        │  passes, exact token           │  not each issue's token
        │  match required                │  (fetch revalidates each issue)
        ▼                                ▼
        └──────────────┬─────────────────┘
                        ▼
          stable parent census
                    │
                    ▼
 comments + attachments + links, fully paginated
        │  consecutive normalized reads must match per parent
        ▼
 SourceItemRef(kind, id, routing facts, composite change token)
                    │
                    ▼
 ConnectorTemplate cursor + router + orchestrator
        │  create / noop / revise / delete, independently per kind
                    ▼
        IronRAG /v1 content + ops endpoints
```

The adapter validates the permanent-token identity before every sweep, then
once more after both snapshots are collected. A changed identity aborts the
sweep before anything is yielded.

Every collection uses explicit `$top`/`$skip` pagination. A duplicate ID or a
non-progressing page fails the sweep; a short page is not treated as complete
until the following request returns empty. `/api/issues` uses
`sort by: created asc, {issue id} asc`: `created` prevents ordinary edits from
reshuffling later offset pages, while `{issue id}` provides a deterministic
tie-breaker.

This ordering is not a transactional snapshot. A cross-project move changes
YouTrack's readable issue ID, so it can still move an issue within an active
offset walk. The client aborts on a duplicate instead of accepting that page,
and the adapter accepts a census only after two complete passes return the same
set of immutable database `Issue.id` values. Each issue is then revalidated
before materialization. These checks make a detected move race retry the sweep;
they do not claim that offset pagination became atomic.

Nor can the final source read and the later IronRAG revision/delete be one
cross-system transaction. An ACL can tighten after the last validation read
but before the target mutation completes. Consecutive reads and `REQUIRED`
cleanup bound detection and remove persistent stale content; they are not a
query-time authorization mechanism and do not claim a zero-duration disclosure
window. The production audience invariant below is therefore required across
every ACL transition allowed for the routed corpus, not merely at one audit
instant.

Articles require two complete parent passes with identical listing tokens.
Issues change too often for that rule, so their two passes require the same ID
set while each issue is revalidated before materialization. Once the parent
census is stable, every eligible parent's comments, attachments, links and
link targets are normalized twice consecutively. If the reads differ, the
adapter retries up to `YOUTRACK_SNAPSHOT_VALIDATION_PASSES`; without a matching
pair it fails that parent instead of combining two source states.

Enumeration constructs article ancestry from the fully paginated article
census. During `fetch()`, the adapter follows the current parent chain with the
minimal descriptor `id,parentArticle(id),visibility($type)`; it does not fetch
ancestor bodies, users, tags or child collections. Each returned database ID
must match the requested ancestor, and only an explicit `parentArticle: null`
marks the root. A missing ancestor, a mismatched or malformed descriptor, an
unknown access decision or a cycle becomes an unknown access-chain entry and
selects `SourceFetchDelete(REQUIRED)`. Only a transport or server failure is
propagated for a later sweep; no ambiguous successful response is treated as a
readable root.

For issues with comment sync enabled, each stable related snapshot must contain
exactly `Issue.commentsCount` comments. The count is shape-checked as a
non-negative integer, included in the parent token and used as a security
gate: a mismatch means the endpoint did not prove a complete readable comment
collection and
requires deletion of the composite target. The article-comments endpoint only
promises all comments accessible to the caller, and `Article` exposes no
independent count. Consecutive reads can prove stability there, not
completeness; full article-comment permission and source/target audience
admission remain explicit operational prerequisites.

`fetch()` rebuilds the same parent and child projection. Security tightening is
checked before ordinary drift: a parent that becomes restricted or unreadable,
an incomplete issue-comment collection, or a previously indexable child that
becomes restricted, deleted or inaccessible returns
`SourceFetchDelete(REQUIRED)` immediately. A changed token, route, project
filter or tag filter with no security tightening returns `SourceFetchDeferred`;
the existing IronRAG document and cursor remain unchanged for the next sweep.
This prevents an A -> B -> A race from being recorded as an empty fetch without
retaining bytes whose readability can no longer be proven.

### Identity and diff

|                | article                                     | issue                                |
| -------------- | -------------------------------------------- | ------------------------------------- |
| Source identity | YouTrack database `Article.id`               | YouTrack database `Issue.id`          |
| IronRAG identity | `youtrack:article:<Article.id>`             | `youtrack:issue:<Issue.id>`           |
| Citation URL   | `<YOUTRACK_BASE_URL>/articles/<idReadable>`  | `<YOUTRACK_BASE_URL>/issue/<idReadable>` |
| MIME           | `text/markdown`                              | `text/markdown`                       |

The token has the form `youtrack-composite-v<projection-version>:<sha256>`.
The digest covers the normalized projection: source timestamps and summary,
safe project/user metadata, allowlisted custom fields, issue `commentsCount`,
access decisions, comments, attachment metadata and eligible issue links.
Comments are ordered by `(created, id)`; all other collections also use stable
identity ordering, so API response order cannot manufacture a revision.

The projection version is also part of the hashed input. Bump it when the
admitted projection, its enforcement semantics or the rendered Markdown change:
the new version changes every article and issue token, including cursors
written before the versioned prefix existed, and forces one complete
rematerialization under the current contract.

The Markdown starts with the current summary and source metadata, followed by
the article content or issue description. Safe custom fields, attachment
descriptors, eligible links and comments follow in named sections. A rendered
attachment descriptor carries its name, MIME type, size, source ID and
created/updated timestamps; its bytes and download URLs never cross the
boundary. A comment records its stable ID, author login, timestamps, pinned
state and text. Empty or null content still produces the heading and identity
block, so clearing a body replaces stale bytes instead of leaving them indexed.

The source summary and citation hint are synchronized inside the Markdown and
in complete replacement metadata. Omitted optional metadata is normalized from
the replacement request, never inherited from a restricted old revision.

### Field admission

[`field_contract.py`](src/ironrag_connector/sources/youtrack/field_contract.py)
lists every requested REST leaf and assigns it to one or more destinations:
identity, indexed content, indexed metadata, routing, the change token, a
security gate, or connector-only service logic. The test parses the actual
projection constants, so a newly requested field fails CI until it receives
one exact row. There is no wildcard row.

`YOUTRACK_CUSTOM_FIELDS` is the only custom-field admission path. The client
sends each configured name through YouTrack's `customFields` query parameter
and never requests all project fields. The adapter enforces the same boundary
on the response: it skips an unconfigured entry before reading or copying its
value, so that value cannot enter `ref.raw`, Markdown, routing facts or the
change token. Configured values normalize as null, scalar, identity object or
sorted list; an unknown shape fails closed.

`SourceItemRef.raw` is not a copy of the vendor response. It contains only the
normalized `_ironrag_projection` needed to compare access and related-child
transitions during `fetch()`. Parent bodies, arbitrary response fields and
custom-field values therefore do not persist in this fetch aid.

Every configured, non-null field may emit one flat routing fact named
`custom_field:<verbatim YouTrack name>`. The adapter does not publish fixed
aliases such as `type`, `state`, `priority` or `assignee_login`, and it does not
slugify or case-fold names. Scalars route as strings; multi-values route as a
sorted string list. Identity objects select `login`, then `name`, then `id`.
Routing YAML must quote a key such as `"custom_field:Product / Area"` and use
the same spelling that appears in `YOUTRACK_CUSTOM_FIELDS`.

The connector intentionally excludes these surfaces:

| Surface | Treatment and reason |
| ------- | -------------------- |
| User email, display name and profile data | Omitted; stable IDs and logins cover identity and routing without copying profile data. |
| Attachment URLs, signed URLs, thumbnails, base64 and binary content | Omitted; only safe name, MIME type, size, source ID and created/updated timestamps are rendered. |
| Reactions, voters, watchers and work items | Omitted because the corpus does not need their identity and activity data. |
| Text previews and wikified HTML | Omitted; canonical Markdown/plain text is indexed once. |
| External integration records | Omitted because their payload and access model are not part of the document contract. |
| Tag sharing metadata | Omitted. Tag names may gate scope or routing, but tag sharing does not prove parent readability. |

The parent attachment endpoint owns the complete attachment collection,
including comment-owned attachments. `attachment.comment.id` assigns each
descriptor exactly once to its comment or to the parent section. Draft,
removed and restricted descriptors are not rendered; bytes and download URLs
never cross the connector boundary.

### Scope: project filters and the tag gate

`YOUTRACK_PROJECTS`/`YOUTRACK_EXCLUDE_PROJECTS`/`YOUTRACK_INCLUDE_ARCHIVED_PROJECTS`
gate both kinds identically -- there is one project scope, not one per kind.
`YOUTRACK_REQUIRE_TAG`, when set, is ANDed on top: only items (either kind)
carrying that exact tag sync, independent of project. It is enforced both
during enumeration and again during `fetch()`'s revalidation, so removing the
tag from a live item behaves exactly like the item leaving its project scope.

Issue-link targets pass the same project, archive and required-tag scope before
their readable IDs are rendered. A link to a target outside that scope stays
out of the indexed parent even when the connector account can read it.

### Visibility and target audience

The YouTrack API exposes neither tombstones nor an activity cursor for either
kind. After a complete stable snapshot, ConnectorTemplate compares each kind's
IDs with its own active documents (`youtrack:article:` / `youtrack:issue:`)
and reconciles what is missing, independently per kind. `YouTrackAdapter`
returns `missing_enforcement=REQUIRED` for both kinds, so a parent absent from a
complete census is deleted even when its configured `on_missing` action is
`ignore`. The exact-key scan covers the current route, durable and pre-sweep
historical routes, and every current routing target.

Every parent must report `UnlimitedVisibility`. YouTrack evaluates that entity
visibility underneath project permissions; it is not a substitute for `Read
Project Basic` and the entity-specific read permissions. An article is admitted
only when every ancestor up to the Knowledge Base root also reports unlimited
visibility. Limited comments and link targets are omitted. Limited attachments
lose their descriptive metadata and are omitted; deleted issue comments are
handled the same way. A missing, malformed or unknown visibility type anywhere
in the parent chain or related collections requires deletion of the parent
document rather than guessing public access.

The connector account requires `Read Project Basic`, `Read Article`, `Read
Article Comment`, `Read Issue` and `Read Issue Comment` for every admitted
project, as well as visibility of every included attachment descriptor and link
target. Issue comment reads are checked against `Issue.commentsCount`. Article
comment reads have no corresponding count and therefore rely on the explicit
permission/audience admission above; their consecutive snapshots establish
stability only.

This check is necessary, but it does not prove access equivalence. YouTrack
project permissions, article ancestors and field-level rules can restrict an
object whose entity visibility says `UnlimitedVisibility`; IronRAG library IAM
is a separate system. A production route is admissible only with operator
evidence that:

```text
Readers(IronRAG target library) is a subset of Readers(YouTrack projected corpus)
```

This is a temporal invariant: every target reader must remain authorized for
every routed object throughout every permitted project/entity ACL transition.
If an object can be narrowed below the target audience, the route must remain
disabled until an ACL-aware query-time mapping or another atomic authorization
boundary exists. `REQUIRED` reconciliation limits stale persistence after a
poll observes the change; it cannot substitute for that boundary.
Entity-level visibility must never be treated as evidence that the whole
project or Knowledge Base corpus is public; reader-set proof is an operational
admission requirement.

Token revocation, guest fallback, malformed JSON, pagination failure or an
unstable parent census aborts enumeration and suppresses the reaper. One
collection failure therefore cannot turn unread source data into mass deletes.

### Fetch outcomes

| Result | Meaning |
| ------ | ------- |
| `SourceFetchMaterialized(REQUIRED)` | Accept independent denial for every existing exact-key copy, retain historical retirement receipts/locations, then create when genuinely absent or submit an exact bound current-route replacement regardless of `on_changed=skip`. Record the source token only after matching READY/current lineage and historical public denial. |
| `SourceFetchDelete(REQUIRED)` | Keeping the indexed copy is unsafe, for example after a visibility transition, post-enumeration unreadability or issue-comment count mismatch; delete even when `on_missing=ignore`. A REQUIRED delete decision forced by limited or unknown visibility carries `exclusion=LIMITED_VISIBILITY`; `SyncReport.skipped_limited_visibility` counts that decision once per sweep, independently of the action (`deleted`/`source_absent`/`skipped_missing`) it produced, and a still-limited entity reproduces the decision on every sweep without another deletion. |
| `SourceFetchDeferred` | The source moved during materialization or cannot be judged consistently; preserve the document and cursor, then retry. |

The full reconciliation sweep remains the only deletion authority. YouTrack
has no complete webhook stream, and an absent item is reaped only after a
complete enumeration. A public-to-restricted transition visible in the API
and a previously visible child that disappears from an accessible-only
collection use `REQUIRED` deletion in the same sweep. A parent that becomes
unreadable after enumeration does too. An item that leaves a project or tag
filter is deferred during that fetch and then deleted as a required
complete-census absence on the next stable sweep.

Both kinds also set `materialization_enforcement=REQUIRED` on their refs and
materialized results. If the current-route revision fails or is deferred, the
orchestrator deletes that stale exact-key document without advancing the
cursor. A cleanup failure is a hard error; after successful cleanup, the next
stable sweep treats the route as absent and, with `on_new=create`, creates a new
safe document with a new IronRAG document ID.

### Request cost and signals

Comment completeness costs requests: two parent census passes are followed by
at least two reads of each eligible parent's related collections. Issues add a
link-target request for every non-empty link bucket. `YOUTRACK_PAGE_SIZE`,
`YOUTRACK_MIN_REQUEST_INTERVAL_SECONDS` and the periodic sync interval must be
sized from the corpus rather than copied from another connector.

The client honors `Retry-After` for 429/5xx responses and emits
`youtrack.retry` with status, attempt and delay. Production monitoring should
track sweep duration, source request count by endpoint/status, 429 and retry
counts, deferred parents, stabilization failures, rendered bytes and comment
count per parent. `SyncReport.skipped_limited_visibility` counts
visibility-forced REQUIRED decisions once per sweep, independently of ordinary
reaps: a still-limited entity reproduces the decision every sweep, even after
its one deletion. A rise in any of
these is a capacity or source-churn signal;
the response is to adjust the interval/throttle or inspect an oversized parent,
not to weaken the consecutive-read check.


---

## Confluence

Confluence ships no architecture note of its own. Its adapter follows the same
shape as BookStack's: pages are enumerated from the REST API, attachments are
dependents of the page fetch, `external_key` is `confluence:<kind>:<id>`, the
change token is the page version, and a webhook handler dispatches through the
same orchestrator the periodic sweep uses. A page's `dependents_token` is
derived from one `type=attachment` CQL walk per sweep (attachment id and
version, grouped by `container` page, restricted to the same space scope as
pages but not to the custom page CQL); an index failure fails the enumeration.
With `CONFLUENCE_INCLUDE_ATTACHMENTS=false` every page's proven dependent set
is empty and the index is not read. A page whose attachment list or download
cannot be read is deferred and keeps its previously proven set, and a download
that returns no bytes counts as such a read; an attachment over
`CONFLUENCE_ATTACHMENT_MAX_BYTES` is the deliberate exclusion that leaves an
attachment out of the proven set.

---

## Invariants

These held in one connector's notes and not in another's; they are contract,
not preference, and they apply to every source.

- **Test data is synthetic.** No vendor hostname, no customer name, no string
  taken from a real corpus, in fixtures or in tests. The repository is public.
- **`external_key` must round-trip.** `parse_external_key(external_key(kind, id))`
  returns `(kind, id)` for every kind a source emits, and returns `None` for a
  key belonging to another source. The orphan reaper deletes by external key —
  a key that does not round-trip deletes the wrong document or nothing at all.
- **`change_token` identifies content, not time.** Git uses the blob sha;
  vendors that expose a version number use that. A wall-clock timestamp makes
  every sweep look like a change.
- **Git is the CLI, not a library binding.** The git source shells out to
  `git`, which is why its image is the only one carrying `git` and
  `openssh-client`.
- **YouTrack is poll-only.** Its Knowledge Base emits no complete lifecycle
  events, so `RUN_MODE` rejects `webhook` and `both` rather than silently
  missing deletions.
- **Primary kinds are proven by the census; dependent kinds by membership.**
  A kind reached only through a parent (an attachment, an inline image) must
  not be declared primary: the reaper would delete it for not appearing in
  `iter_items`. It is reaped instead against the durable claims its parents'
  complete fetches prove, and only once every enumerated parent is proven.
  A dependent tuple is the parent's complete set: an adapter that could not
  read one dependent defers the parent rather than omitting it.
