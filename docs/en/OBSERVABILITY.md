# Logs, errors and error tracking

This document is the log record contract for IronRAG. It covers the structured
OTLP log sink, the error record shape, the fingerprint an operator groups on,
and the redaction guarantees. The stdout formatter stays plain text and is not
the structured contract.

## Log field contract

Every record exported through OpenTelemetry carries:

| Field | Meaning |
| --- | --- |
| `severity_text` / `severity_number` | The `tracing` level (`INFO`, `WARN`, `ERROR`, ...). |
| `service.name` | `ironrag-api` or `ironrag-worker`. |
| `service.version` | The release the process runs (`0.6.0` at the time of writing). |
| `deployment.environment.name` | The deployment environment. |
| `service.instance.id` | The process instance identity. |
| `request_id` | The request correlation id, also returned in `x-request-id`. |
| `trace_id` / `span_id` | The OpenTelemetry span that emitted the record. |

The connectors run outside the cluster without an OTLP exporter; they write one
JSON object per record to stdout with `service = "ironrag-connector"`,
`release`, `level`, `timestamp`, `event`, a per-sweep `run_id` and a per-item
`request_id`, plus the fields of the emitted event.

`request_id` is generated or accepted once per HTTP request and written to the
request, the response header and the `http_request` span, so the start,
completion and failure lines of one request agree. The response also carries
`x-trace-id` with the request's trace id, so a user-reported failure can be
joined to its trace without server access. Records emitted inside an active span
carry `trace_id` and `span_id`; a record outside any span carries neither. The
ingest worker runs one `ingest.job` span per leased attempt with
`ingest.job.id`, `ingest.job.kind`, `ingest.attempt.id`, `stage`, `run_id` (when
the job kind resolves a run) and the workspace/library identity, so nested
stage, provider and vector-DB records are joinable to that attempt.

## Error records

A terminal failure is emitted as one record with `event = "error"`:

| Field | Meaning |
| --- | --- |
| `error.type` | Stable fault class, e.g. `ingest.attempt_failed`, `query.execution_failed`, `llm.provider_call`, `http.server_error`. |
| `error.message` | Redacted message, bounded to 512 characters. |
| `error.fingerprint` | 16 lowercase hex characters identifying the fault class. |
| `error.stack` | The backtrace when `RUST_BACKTRACE` enables it, otherwise the literal `disabled`. |
| `error.occurrences` | How many identical faults this record stands for. |

A panic is emitted as `event = "panic"` with `error.type = "panic"`,
`error.message`, `error.fingerprint`, `error.stack` (always captured),
`error.location` (`file:line:column`) and `error.release`. The previous panic
hook still runs. A panic is never deduplicated.

Repeats of one fingerprint inside a 60-second window are suppressed and
counted. The next emission of that fingerprint reports the accumulated count in
`error.occurrences`. The process tracks at most 1024 fingerprints; the oldest
entry is evicted when the map is full.

The 5xx completion line carries `error.type = "http.server_error"` and a
fingerprint derived from the matched route and the status code, so a broken
endpoint groups without conflating `500` and `502`.

## Fingerprint

`error.fingerprint` is 16 lowercase hex characters of SHA-256 over the fault
class and a normalised message. Normalisation lowercases the text and replaces
UUIDs, hex ids, email addresses, URLs, quoted paths and numbers with stable
tokens, so two occurrences of one fault share an id even when the ids, counts
and paths differ. The fingerprint is computed from the redacted message, so a
secret never influences grouping.

Group repeats in VictoriaLogs:

```text
{event="error"} | stats by (error.fingerprint) count()
```

## Redaction guarantees

`error.message`, `error.stack` and every connector string value pass through one
redaction rule set before the record is emitted. The whole matched secret is
replaced with a fixed marker, so the reader knows which class was removed:

| Class | Marker |
| --- | --- |
| `Bearer <token>` | `[REDACTED:bearer]` |
| `Basic <base64>` | `[REDACTED:basic]` |
| Provider API key (`sk-...`) | `[REDACTED:provider_key]` |
| `password`, `secret`, `token`, `api_key` and siblings with a value | `[REDACTED:<key>]` |
| Credentials in a URL (`scheme://user:password@host`) | `[REDACTED:credentials]` |
| Email address | `[REDACTED:email]` |

Scrubbed values are bounded to 4096 characters with a `[TRUNCATED]` suffix; an
error message is additionally bounded to 512 characters. Business fields are
not rewritten: only log messages, stacks and connector log values pass through
the redactor. The redacted provider detail token persisted for operators
(`provider=... http_status=...`) is unchanged by this layer.

## Alert and runbook

The owning alert is `ironrag_error_fingerprints` ("IronRAG повторяет одну и ту
же ошибку", group `IronragLogAlerts`) in the Grafana monitoring repository,
provisioned in `grafana/provisioning/alerting/ironrag-log-rules.yml`. It counts
`event="error"` records for `ironrag-api` and `ironrag-worker` over five
minutes, grouped by `error.fingerprint`, and fires when one class exceeds three
records and stays above it for ten minutes. Its owner label is
`piping-space/products/ironrag`; its runbook is
<https://youtrack.piping.space/articles/OPS-A-28>.
