<div align="center">

# IronRAG Webhooks

### Outbound webhooks: broadcast revision-ready and document-deleted events to subscribers

[Overview](./README.md) | [Webhooks (RU)](../ru/WEBHOOK.md) | [Credential encryption](./CREDENTIAL-ENCRYPTION.md) | [MCP](./MCP.md) | [IAM](./IAM.md) | [CLI](./CLI.md)

</div>

## Overview

IronRAG sends outbound webhooks to notify external systems about state changes. Inbound processing of vendor events (Confluence, MediaWiki, Notion, etc.) is the responsibility of an external middleware layer that calls IronRAG's existing upload/replace/delete HTTP API directly.

**Outbound** — `webhook_subscription` registers HTTPS endpoints that receive HMAC-signed events about IronRAG state changes (`revision.ready`, `document.deleted`). Delivery is durable: every send is an `ingest_job` with `job_kind=webhook_delivery`, and the existing worker pool handles lease/heartbeat/retry semantics. Failures are retried with exponential backoff up to 8 attempts (maximum delay 128 minutes), then marked `abandoned`.

## Subscription model

`webhook_subscription` rows describe HTTP destinations that receive IronRAG events.

```
POST /v1/webhooks/subscriptions
Authorization: Bearer <api-token with workspace_admin>
Content-Type: application/json

{
  "workspaceId": "<uuid>",
  "libraryId": "<uuid or null for workspace-wide>",
  "displayName": "Downstream index",
  "targetUrl": "https://hooks.example.com/ironrag-events",
  "secret": "<random 32+ byte hex>",
  "eventTypes": ["revision.ready", "document.deleted"],
  "customHeaders": {
    "Authorization": "Bearer <receiver-token>"
  }
}
```

| Field | Notes |
|-------|-------|
| `workspaceId` | Scope. Only events from this workspace are dispatched |
| `libraryId` | Optional. If set, only events from this library; null = all libraries in the workspace |
| `eventTypes` | Non-empty array of event names |
| `secret` | HMAC-SHA256 key for outgoing signatures |
| `customHeaders` | Up to 32 validated string headers, encrypted at rest. Hop-by-hop, body-framing, signature, and trace-context headers are reserved |
| `active` | Defaults to `true`; set `false` to pause |

Each workspace supports at most 100 active subscriptions. A workspace-scoped
database trigger serializes create/reactivation at the transaction boundary, so
concurrent requests and older rolling-deployment pods share the same fanout
bound; inactive subscriptions do not consume it. A redacted audit view reports
any pre-existing over-quota tenant. Active and inactive rows combined are
limited to 1,000 per workspace. IronRAG never silently age-purges inactive
subscriptions because their delivery rows are operational audit evidence;
delete inactive subscriptions explicitly before creating more.

CRUD endpoints:

- `GET    /v1/webhooks/subscriptions?workspaceId=`
- `GET    /v1/webhooks/subscriptions/{id}`
- `POST   /v1/webhooks/subscriptions`
- `PATCH  /v1/webhooks/subscriptions/{id}`
- `DELETE /v1/webhooks/subscriptions/{id}`
- `GET    /v1/webhooks/subscriptions/{id}/attempts`

Both list endpoints use bounded `(createdAt, id)` keyset pagination while
keeping their existing JSON-array response shape. Subscription pages default
to 100 items; delivery-attempt pages default to 200; both clamp `limit` to 200.
To continue, copy `createdAt` and `id` from the final item into the paired
`afterCreatedAt` and `afterId` query parameters. Supplying only one component
returns `400`. Subscriptions retain oldest-first ordering; attempts retain
newest-first ordering. Management projections never load signing secrets,
custom-header ciphertext, signed event payloads, response bodies, queue job
identifiers, or delivery lease tokens.

Global subscription UUID routes are filtered by the caller's authorized
workspace scope in SQL. A missing subscription and a subscription in another
tenant both return the same `404`, including GET, PATCH, DELETE, and the attempt
list, so UUIDs cannot be used for tenant enumeration.

`DELETE` returns `204` only after no claimed delivery owner remains. If a POST
is already owned, the first call durably tombstones the subscription and returns
`202 Accepted`; retry the same DELETE until it returns `204`. A tombstoned
subscription cannot be reactivated through PATCH. Lease age alone is not proof
that an external HTTP side effect has stopped.

Delivery is at least once: a worker can lose its database lease after the
remote endpoint accepted a POST but before the result was persisted. Receivers
must deduplicate idempotently by `X-IronRAG-Event-Id` (the same value as
`event_id` in the body). Database lease tokens fence stale result writes; they
cannot make an external HTTP side effect exactly once. A crashed draining owner
therefore requires the explicit operator command
`ironrag-maintenance repair webhook-delivery-abandon --subscription <uuid> --acknowledge-duplicate-delivery-risk`; ordinary DELETE never infers acknowledgement from lease age.

## Event catalog

### `revision.ready`

Fired after the ingest pipeline finishes a revision and promotes it to readable. Sent for every successful upload, replace, append, or edit.

```json
{
  "event_type": "revision.ready",
  "event_id": "revision.ready:<revision_uuid>",
  "occurred_at": "2026-04-25T12:30:42Z",
  "workspace_id": "<uuid>",
  "library_id": "<uuid>",
  "document_id": "<uuid>",
  "revision_id": "<uuid>"
}
```

### `document.deleted`

Recorded atomically with the persisted soft-delete transition and becomes
eligible for delivery only after that transaction commits. Projection cleanup
is independent and cannot make the durable lifecycle event disappear.

```json
{
  "event_type": "document.deleted",
  "event_id": "document.deleted:<document_uuid>:<deleted_at_unix_microseconds>",
  "occurred_at": "2026-04-25T12:32:10Z",
  "workspace_id": "<uuid>",
  "library_id": "<uuid>",
  "document_id": "<uuid>"
}
```

## Outgoing signature scheme

Every outbound POST carries:

```
Content-Type: application/json
X-Ironrag-Signature: t=<unix_seconds>,v1=<hex_hmac_sha256>
X-Ironrag-Event-Type: revision.ready
X-Ironrag-Event-Id: revision.ready:<uuid>
```

Plus any `customHeaders` configured on the subscription.

The raw signed body is the flat JSON object shown in the event catalog. IronRAG
overwrites `event_type`, `event_id`, `occurred_at`, `workspace_id`, and
`library_id` with canonical persisted metadata before enqueueing, so a producer
payload cannot spoof routing or deduplication fields.

The signature input is `<ts_unix_seconds>.<raw body bytes>` — the `.` is literal. The HMAC key is `subscription.secret`.

### Verifying received events (receiver side)

```python
import hmac, hashlib, time

def verify(secret: bytes, header: str, body: bytes, window_seconds: int = 300) -> bool:
    try:
        parts = dict(p.split("=", 1) for p in header.split(","))
        ts = int(parts["t"])
        received_mac = parts["v1"]
    except (KeyError, ValueError):
        return False
    if abs(time.time() - ts) > window_seconds:
        return False  # replay window exceeded
    expected = hmac.new(secret, f"{ts}.".encode() + body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(expected, received_mac)
```

**Do not re-serialize the body** between receiving and verifying; bytes must match exactly.

## Retry policy

| Outcome | Behaviour |
|---------|-----------|
| HTTP 2xx | Delivery marked `delivered`, `delivered_at` set |
| HTTP 5xx, 429, network/timeout | `attempt_number++`; attempts 1–7 schedule delays of 2, 4, 8, 16, 32, 64, and 128 minutes. Attempt 8 → `abandoned` |
| HTTP 4xx (other) | Marked `failed`, no retry |

Replay protection: receivers SHOULD reject deliveries whose `t=` is outside ±5 minutes of their clock.

### Delivery-fencing upgrade note

The expansion migration adds token-fenced completion without rewriting or
reclaiming a legacy tokenless in-flight owner: that old process may still
resume and persist its result during rolling overlap. Upgraded workers reclaim
only leases already created with a current-protocol token. A persistent
database trigger redacts response bodies and dynamic legacy error strings, and
a deferred commit trigger supplies a correctly scoped queue job when the old
two-transaction publisher commits an unlinked pending attempt.

A crashed tokenless owner remains fail-closed and must be resolved with the
explicit risk-acknowledging abandon command documented above. After old writers
are drained, operators verify the redacted lease/tenant/contract audit views;
only a later contract migration may validate the restrictive lease-shape and
subscription field checks.

## Catalog deletion safety contract

Library and workspace deletion are fail-closed around durable webhook work. A library delete
locks its `catalog_library` row. A workspace delete locks the `catalog_workspace` row and every
child library row. The transaction then refuses to delete while either of these conditions is
true anywhere in the deletion scope:

- a `webhook_lifecycle_outbox` row in scope is `pending`, `dispatching`, or
  `dead_letter`; explicit terminal states `dispatched` and `resolved` do not
  block deletion; or
- a `webhook_delivery` ingest job in scope is `queued`, `leased`, `paused`, or `failed`; or
- a delivery attempt is still `pending`/`delivering`, or is retryable `failed`,
  but has no matching active queue job (including the old publisher's
  create-before-link crash window).

Only after those rows are drained does the same transaction perform the catalog delete. The parent
locks also fence concurrent FK-backed library/outbox/job inserts: an insert cannot slip between the
guard and the cascading delete and silently erase an event. A blocked execution maps to a typed
catalog conflict (`409` on synchronous surfaces). The REST catalog delete route is admitted
asynchronously with `202`; if work becomes blocked before its worker executes, the async operation finishes as
failed and deletion can be retried after the outbox/job is drained. In particular, a
`dead_letter` event requires an explicit operator choice and is never discarded merely by deleting
its library or workspace. A `resolved` row is non-blocking, while its redacted global audit event
survives the subsequent catalog cascade.

For the current `ingest_queue_state` enum, only `completed` and `canceled` webhook-delivery jobs
are non-blocking. `abandoned` is a delivery-attempt state, not an ingest-job queue state. Any future
or otherwise unrecognized queue state is treated as unresolved and blocks deletion by default.

## Lifecycle outbox dead-letter operations

Use the bounded maintenance audit instead of querying secret-bearing webhook tables directly:

```bash
ironrag-maintenance audit webhook-outbox --state dead-letter --limit 100
ironrag-maintenance audit webhook-outbox --state dead-letter --library <library-uuid> --json
```

Valid state filters are `pending`, `dispatching`, `dispatched`, `dead-letter`, and `resolved`; the default is
`dead-letter`, and the command rejects limits outside `1..=500`. Output is deliberately limited to
outbox identity, event type, workspace/library scope, state/attempt counters, typed failure and
resolution reason codes, and timestamps. It
never emits event payloads, event IDs, endpoint URLs, signing secrets, custom headers, lease
identities/tokens, or raw failure text.

When another page exists, JSON includes `has_more=true` and a `next_cursor` object; human output
prints the exact continuation flags. Continue with both keyset components and the same filters:

```bash
ironrag-maintenance audit webhook-outbox --state dead-letter \
  --before-created-at <next_cursor.created_at> --before-id <next_cursor.id>
```

This stable keyset pagination lets operators inspect every row without unbounded reads; supplying
only one cursor component is rejected.

After diagnosing and fixing the receiver or configuration, requeue one exact outbox UUID:

```bash
ironrag-maintenance repair webhook-outbox-dead-letter --outbox <outbox-uuid> --json
```

This is an atomic compare-and-set from `dead_letter` to `pending`. It resets attempts, lease fields,
raw error state, and makes the row immediately available. The command performs no HTTP request;
the normal worker loop delivers it later. Missing rows and rows whose state changed are left
untouched and produce a non-zero exit. Audit output includes only a stable typed `last_error_code`;
payloads and raw transport errors are never loaded into the maintenance process.

If delivery is permanently unnecessary, resolve the exact dead-letter without pretending that the
remote endpoint received it:

```bash
ironrag-maintenance repair webhook-outbox-dead-letter-resolve \
  --outbox <outbox-uuid> --reason-code receiver_retired \
  --acknowledge-not-delivered --json
```

This is a separate atomic compare-and-set from `dead_letter` to `resolved`. The reason must be a
bounded lowercase `snake_case` machine code (1–64 ASCII bytes); free-form text is rejected so the
operator cannot accidentally persist a URL, credential, or response body. The row records
`resolution_reason_code` and database-clock `resolved_at`, and the same statement appends a
redacted `webhook.lifecycle_outbox.dead_letter_resolved` audit event. It never changes a
`dispatched` row and never claims that delivery succeeded. Requeue remains the correct choice when
delivery should still happen. The explicit `--acknowledge-not-delivered` flag is mandatory.

## Image-only change detection

When a PDF, DOCX, or PPTX swaps an embedded picture without changing OCR-extractable text, the existing `text_checksum` would be unchanged and the standard chunk-reuse plan would skip re-embedding. To correct this, IronRAG also computes a revision-level `image_checksum` (sorted extracted image bytes, then SHA-256). When `parent.image_checksum != new.image_checksum`, the chunk-reuse plan is bypassed and embeddings + graph extraction recompute fully for that revision. `text_checksum` semantics are preserved (text-only).

## Operational notes

- **Secrets** and serialized **custom header values** are stored as separate authenticated, row-bound `ironrag:enc:v3` AEAD envelopes in `webhook_subscription.secret` and `webhook_subscription.custom_headers_json`. They are decrypted only inside delivery. Each envelope authenticates its purpose, subscription UUID, and key ID. Existing installations must first deploy the dual-reader with encrypted writes disabled, then enable writes in a separate rollout. Master-key rotation requires a three-phase overlapping keyring. Follow the complete [credential encryption runbook](./CREDENTIAL-ENCRYPTION.md); an unknown key ID or failed authentication is always fail-closed.
- **Job queue** is shared with the ingest pipeline. `job_kind=webhook_delivery` competes with `content_mutation`, `web_discovery`, and `web_materialize_page` for worker leases. Heavy outbound load can throttle ingest; tune `IRONRAG_INGESTION_WORKER_POOL_SIZE` accordingly.
- **Lifecycle relay lease**: the relay leases only one outbox row at a time and renews its five-minute lease every 60 seconds with a PostgreSQL-clock, token-fenced CAS. Losing the lease or the renewal query cancels the remaining recipient fanout; deterministic per-recipient dedupe makes the next owner converge safely.
- **Observability**: every outbound attempt is recorded in `webhook_delivery_attempt` and queryable via SQL for forensics. Workers emit `tracing` spans on stage `webhook_delivery`. OTLP metrics include `ironrag.webhook.lifecycle_outbox.event_age_seconds`, `drain_duration_seconds`, `lease_conflicts`, `lease_renewals`, and `outcomes`.

## Reference: outbound example

IronRAG fires `revision.ready` when a document finishes ingesting. A subscriber forwards the event to a downstream search index:

```python
import hmac, hashlib, time, json, requests

def verify_and_forward(secret: bytes, header: str, body: bytes):
    # Verify IronRAG signature
    parts = dict(p.split("=", 1) for p in header.split(","))
    ts = int(parts["t"])
    expected = hmac.new(secret, f"{ts}.".encode() + body, hashlib.sha256).hexdigest()
    assert hmac.compare_digest(expected, parts["v1"]), "bad signature"
    assert abs(time.time() - ts) < 300, "replay window exceeded"

    event = json.loads(body)
    if event["event_type"] == "revision.ready":
        requests.post(
            "https://search.internal/ingest",
            json={"document_id": event["document_id"], "library_id": event["library_id"]},
            timeout=10,
        )
```

For vendor inbound (Confluence page updated → IronRAG document replaced), see the external middleware project — IronRAG's upload/replace/delete API is the entry point.
