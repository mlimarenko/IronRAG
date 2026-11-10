# IronRAG SLO and alert contract

Owner: `piping-space/products/ironrag`. Alert rules live in
`piping-space/infrastructure/grafana` (`grafana/provisioning/alerting/ironrag-rules.yml`).
The incident runbook is IRG-A-16.

## Scope

The contract covers the API surface, ingest worker, queues, the PostgreSQL
pools and outbound provider calls. It uses the metrics defined by
`apps/api/src/observability/metrics.rs` and the `/metrics` scrape of both
roles. Provider cost is an operational signal, not an SLO.

## SLOs

| SLO | SLI | Target | Budget window |
| --- | --- | --- | --- |
| Availability | non-5xx share of `axum_http_requests_total` | 99.5% | 30 days |
| Latency | p95 of `axum_http_requests_duration_seconds` | <= 1 s | 30 days |
| Ingest success | `outcome="succeeded"` share of terminal ingest attempts | 99% | 7 days |
| Provider latency | p95 of `ironrag.provider.http_duration` | <= 30 s | 30 days |

`ironrag.provider.http_duration` and `ironrag.db.pool.connections` are sampled
per call and on the maintenance cadence. A missing series means no observed
call or no sample, not success; every alert that treats absence as failure
declares `noDataState: Alerting` explicitly.

## Windows

- **5m**: fast degradation. Used by the latency, provider-latency and database
  pool alerts. Current scrape interval is 30 s, so the window holds at least
  ten samples.
- **24h and 7d**: slow burn. The availability burn alert requires both windows
  to exceed the 0.5% error-budget burn for one hour, so a single short spike
  does not page and a sustained regression is not hidden by a quiet day.
- **30d**: the availability and latency SLO budget. The dashboard shows the
  current 5m/24h/7d values; the 30d budget is evaluated from the same queries
  with a 30d range when an operator checks compliance.

The new alert queries use explicit `[5m]`, `[24h]` and `[7d]` selectors so
each window can be replayed against VictoriaMetrics unchanged; existing rules
keep their current windows. Do not widen a window or raise a threshold to make
a fired rule green; record the incident and fix the signal or the service.

## Alert mapping

| Alert UID | Signal | Owner | Runbook |
| --- | --- | --- | --- |
| `ironrag_scrape_targets_down` | scrape target absent | ironrag | IRG-A-16 |
| `ironrag_http_5xx` | any 5xx for 10m | ironrag | IRG-A-16 |
| `ironrag_slo_availability_burn` | 24h and 7d error budget burn | ironrag | IRG-A-16 |
| `ironrag_slo_latency_p95` | HTTP p95 above 1 s | ironrag | IRG-A-16 |
| `ironrag_ingest_failure_slo` | terminal ingest failures above 1% | ironrag | IRG-A-16 |
| `ironrag_db_pool_saturation` | pool active/max above 90% | ironrag | IRG-A-16 |
| `ironrag_provider_http_latency_p95` | provider HTTP p95 above 30 s | ironrag | IRG-A-16 |

Queue backlog, lease recovery, provider 429, cost anomaly and vector lifecycle
alerts keep their existing UIDs and thresholds. New rules must not reuse an
existing UID or restate an existing signal with a looser threshold.

## Verification

```promql
# Availability over each window
1 - (sum(rate(axum_http_requests_total{job="ironrag",status=~"5.."}[5m])) or vector(0))
    / clamp_min(sum(rate(axum_http_requests_total{job="ironrag"}[5m])), 1e-9)

# Latency p95
histogram_quantile(0.95, sum by (le) (rate(axum_http_requests_duration_seconds_bucket{job="ironrag"}[5m])))

# PostgreSQL pool saturation
max by (pool) ({__name__="ironrag.db.pool.connections", state="active"})
  / clamp_min(max by (pool) ({__name__="ironrag.db.pool.connections", state="max"}), 1)
```

Run the provisioned expressions against VictoriaMetrics for 5m, 24h and 7d.
A healthy service returns a value or no series, never an execution error.

## Remaining gaps

- No per-library or per-tenant SLO: identifiers are unbounded and stay out of
  metric labels.
- `ironrag.provider.http_duration` measures the time to response headers for a
  single attempt, not body streaming or token generation.
- PostgreSQL pool counters are published by the worker process; an API-only
  deployment does not publish them yet.
