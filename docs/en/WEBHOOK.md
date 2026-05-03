<div align="center">

# IronRAG Webhooks

### Outbound webhooks: broadcast revision-ready and document-deleted events to subscribers

[Overview](./README.md) | [Webhooks (RU)](../ru/WEBHOOK.md) | [MCP](./MCP.md) | [IAM](./IAM.md) | [CLI](./CLI.md)

</div>

## Overview

IronRAG sends outbound webhooks to notify external systems about state changes. Inbound processing of vendor events (Confluence, MediaWiki, Notion, etc.) is the responsibility of an external middleware layer that calls IronRAG's existing canonical upload/replace/delete HTTP API directly.

**Outbound** — `webhook_subscription` registers HTTPS endpoints that receive HMAC-signed events about IronRAG state changes (`revision.ready`, `document.deleted`). Delivery is durable: every send is an `ingest_job` with `job_kind=webhook_delivery`, and the existing worker pool handles lease/heartbeat/retry semantics. Failures are retried with exponential backoff up to 8 attempts (cap 6 hours), then marked `abandoned`.

## Subscription model

`webhook_subscription` rows describe HTTP destinations that receive IronRAG events.

```
POST /v1/webhooks/subscriptions
Authorization: Bearer <api-token with workspace_admin>
Content-Type: application/json

{
  "workspace_id": "<uuid>",
  "library_id": "<uuid or null for workspace-wide>",
  "display_name": "Internal search index",
  "target_url": "https://search.internal/ingest/ironrag-events",
  "secret": "<random 32+ byte hex>",
  "event_types": ["revision.ready", "document.deleted"],
  "custom_headers": {
    "X-Internal-Routing": "ironrag-prod"
  }
}
```

| Field | Notes |
|-------|-------|
| `workspace_id` | Scope. Only events from this workspace are dispatched |
| `library_id` | Optional. If set, only events from this library; null = all libraries in the workspace |
| `event_types` | Non-empty array of canonical event names |
| `secret` | HMAC-SHA256 key for outgoing signatures |
| `custom_headers` | Free-form HTTP headers added to every delivery |
| `active` | Defaults to `true`; set `false` to pause |

CRUD endpoints:

- `GET    /v1/webhooks/subscriptions?workspace_id=`
- `GET    /v1/webhooks/subscriptions/{id}`
- `POST   /v1/webhooks/subscriptions`
- `PATCH  /v1/webhooks/subscriptions/{id}`
- `DELETE /v1/webhooks/subscriptions/{id}`
- `GET    /v1/webhooks/subscriptions/{id}/attempts`

## Canonical events

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
  "revision_id": "<uuid>",
  "source_uri": "<display URL, if any>"
}
```

### `document.deleted`

Fired after a soft delete commits and post-commit cleanup completes.

```json
{
  "event_type": "document.deleted",
  "event_id": "document.deleted:<document_uuid>",
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

Plus any `custom_headers` configured on the subscription.

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
| HTTP 5xx, 429, network/timeout | `attempt_number++`, `next_attempt_at = now + 2^min(attempt_number, 8)` minutes (cap 6 h). After 8 attempts → `abandoned` |
| HTTP 4xx (other) | Marked `failed`, no retry |

Replay protection: receivers SHOULD reject deliveries whose `t=` is outside ±5 minutes of their clock.

## Image-only change detection

When a PDF, DOCX, or PPTX swaps an embedded picture without changing OCR-extractable text, the existing `text_checksum` would be unchanged and the canonical chunk-reuse plan would skip re-embedding. To correct this, IronRAG also computes a revision-level `image_checksum` (canonical sort of all extracted image bytes, then SHA-256). When `parent.image_checksum != new.image_checksum`, the chunk-reuse plan is bypassed and embeddings + graph extraction recompute fully for that revision. `text_checksum` semantics are preserved (text-only).

## Operational notes

- **Secrets** are stored in plaintext in `webhook_subscription.secret`. Encryption at rest is on the roadmap; treat the database as a credentials store today.
- **Job queue** is shared with the ingest pipeline. `job_kind=webhook_delivery` competes with `content_mutation`, `web_discovery`, and `web_materialize_page` for worker leases. Heavy outbound load can throttle ingest; tune `IRONRAG_INGESTION_WORKER_POOL_SIZE` accordingly.
- **Observability**: every outbound attempt is recorded in `webhook_delivery_attempt` and queryable via SQL for forensics. Workers emit `tracing` spans on stage `webhook_delivery`.

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

For vendor inbound (Confluence page updated → IronRAG document replaced), see the external middleware project — IronRAG's upload/replace/delete API is the canonical entry point.
