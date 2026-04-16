#!/usr/bin/env python3
"""
Canonical UI perf probe.

Reads the curated list of read-only UI endpoints, hits each one against a
running IronRAG instance on a reference library, then pulls the Prometheus
histogram off `/metrics` to compute p50/p95 per endpoint, plus the raw
`time_total` / `size_download` measured at the client.

Writes a markdown report to `tmp/ui-endpoint-profile-<timestamp>.md`.

Usage:
    make perf-probe
    # or
    python3 scripts/ops/profile-ui-endpoints.py \\
        --base-url http://localhost:19000 \\
        --metrics-url http://localhost:9464 \\
        --library-id <library-uuid> \\
        --login admin --password rustrag123 \\
        --runs 5

The script is READ-ONLY: it never calls POST/PUT/DELETE endpoints. Mutating
surfaces (upload, delete, batch rerun, snapshot import) are profiled
separately in their own runbooks.

Thresholds (configurable via CLI):
    ok    p95 < 300ms  AND  bytes < 500 KiB
    slow  p95 < 1000ms AND  bytes < 5 MiB
    huge  anything above
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Iterable

# --- Probe target definition ----------------------------------------------
#
# Each entry lists a UI-reachable GET endpoint plus the template string as it
# appears on the backend router (i.e. the `endpoint` label that
# `axum-prometheus` will emit). Query params are filled with --library-id,
# --document-id, --entity-id at runtime.
#
# Mutating endpoints (POST/PUT/DELETE) are intentionally excluded — they need
# their own write fixtures and are out of scope for a read-only probe.
# --------------------------------------------------------------------------

@dataclass(frozen=True)
class Probe:
    label: str
    matched_path: str
    uri_template: str  # with {library_id}, {document_id}, {entity_id} placeholders


PROBES: list[Probe] = [
    # App bootstrap
    Probe(
        "session.resolve",
        "/v1/iam/session/resolve",
        "/v1/iam/session/resolve",
    ),
    Probe(
        "version.update",
        "/v1/version/update",
        "/v1/version/update",
    ),
    # Dashboard
    Probe(
        "ops.library.state",
        "/v1/ops/libraries/{library_id}",
        "/v1/ops/libraries/{library_id}",
    ),
    Probe(
        "ops.library.dashboard",
        "/v1/ops/libraries/{library_id}/dashboard",
        "/v1/ops/libraries/{library_id}/dashboard",
    ),
    # Documents
    Probe(
        "documents.list.default",
        "/v1/content/documents",
        "/v1/content/documents?libraryId={library_id}&limit=50",
    ),
    Probe(
        "documents.list.with_total",
        "/v1/content/documents",
        "/v1/content/documents?libraryId={library_id}&limit=50&includeTotal=true",
    ),
    Probe(
        "documents.list.page_100",
        "/v1/content/documents",
        "/v1/content/documents?libraryId={library_id}&limit=100",
    ),
    Probe(
        "documents.list.search",
        "/v1/content/documents",
        "/v1/content/documents?libraryId={library_id}&search=pdf&limit=50",
    ),
    Probe(
        "documents.list.status_failed",
        "/v1/content/documents",
        "/v1/content/documents?libraryId={library_id}&status=failed&limit=50",
    ),
    Probe(
        "documents.detail",
        "/v1/content/documents/{document_id}",
        "/v1/content/documents/{document_id}",
    ),
    Probe(
        "documents.head",
        "/v1/content/documents/{document_id}/head",
        "/v1/content/documents/{document_id}/head",
    ),
    Probe(
        "documents.prepared_segments",
        "/v1/content/documents/{document_id}/prepared-segments",
        "/v1/content/documents/{document_id}/prepared-segments?limit=50",
    ),
    Probe(
        "documents.technical_facts",
        "/v1/content/documents/{document_id}/technical-facts",
        "/v1/content/documents/{document_id}/technical-facts?limit=50",
    ),
    Probe(
        "documents.revisions",
        "/v1/content/documents/{document_id}/revisions",
        "/v1/content/documents/{document_id}/revisions",
    ),
    Probe(
        "billing.library_document_costs",
        "/v1/billing/library-document-costs",
        "/v1/billing/library-document-costs?libraryId={library_id}",
    ),
    # Web ingest
    Probe(
        "web_runs.list",
        "/v1/content/web-runs",
        "/v1/content/web-runs?libraryId={library_id}",
    ),
    # Graph
    Probe(
        "knowledge.library.graph",
        "/v1/knowledge/libraries/{library_id}/graph",
        "/v1/knowledge/libraries/{library_id}/graph",
    ),
    Probe(
        "knowledge.library.summary",
        "/v1/knowledge/libraries/{library_id}/summary",
        "/v1/knowledge/libraries/{library_id}/summary",
    ),
    Probe(
        "knowledge.library.entities",
        "/v1/knowledge/libraries/{library_id}/entities",
        "/v1/knowledge/libraries/{library_id}/entities",
    ),
    Probe(
        "knowledge.library.relations",
        "/v1/knowledge/libraries/{library_id}/relations",
        "/v1/knowledge/libraries/{library_id}/relations",
    ),
    Probe(
        "knowledge.library.graph_workbench",
        "/v1/knowledge/libraries/{library_id}/graph-workbench",
        "/v1/knowledge/libraries/{library_id}/graph-workbench",
    ),
    # Assistant
    Probe(
        "query.sessions.list",
        "/v1/query/sessions",
        "/v1/query/sessions?libraryId={library_id}",
    ),
    # Admin
    Probe(
        "admin.surface",
        "/v1/admin/surface",
        "/v1/admin/surface",
    ),
    Probe(
        "catalog.workspaces.list",
        "/v1/catalog/workspaces",
        "/v1/catalog/workspaces",
    ),
    Probe(
        "iam.tokens.list",
        "/v1/iam/tokens",
        "/v1/iam/tokens",
    ),
    Probe(
        "ai.providers.list",
        "/v1/ai/providers",
        "/v1/ai/providers",
    ),
    Probe(
        "ai.prices.list",
        "/v1/ai/prices",
        "/v1/ai/prices",
    ),
    Probe(
        "audit.events.list",
        "/v1/audit/events",
        "/v1/audit/events?limit=50",
    ),
    # Health
    Probe(
        "ready",
        "/v1/ready",
        "/v1/ready",
    ),
]


# --- HTTP helpers ----------------------------------------------------------

class CurlClient:
    """Thin curl wrapper: persistent cookie jar, per-request timing."""

    def __init__(self, base_url: str) -> None:
        self.base_url = base_url.rstrip("/")
        self.cookie_jar = tempfile.NamedTemporaryFile(
            prefix="ironrag-perf-cookies-", delete=False
        ).name

    def login(self, login: str, password: str) -> None:
        body = json.dumps({"login": login, "password": password})
        args = [
            "curl", "-s", "-c", self.cookie_jar,
            "-X", "POST",
            "-H", "Content-Type: application/json",
            "--data", body,
            f"{self.base_url}/v1/iam/session/login",
        ]
        proc = subprocess.run(args, capture_output=True, check=False)
        if proc.returncode != 0 or b"sessionId" not in proc.stdout:
            raise RuntimeError(
                f"login failed: exit={proc.returncode} body={proc.stdout[:200]!r}"
            )

    def request(self, uri: str) -> tuple[int, float, int]:
        """GET uri, return (status, time_total_s, size_download_bytes)."""
        args = [
            "curl", "-s", "-b", self.cookie_jar,
            "-o", "/dev/null",
            "-w", "%{http_code} %{time_total} %{size_download}",
            f"{self.base_url}{uri}",
        ]
        proc = subprocess.run(args, capture_output=True, check=False)
        if proc.returncode != 0:
            raise RuntimeError(
                f"curl failed for {uri}: exit={proc.returncode} "
                f"err={proc.stderr[:200]!r}"
            )
        parts = proc.stdout.decode().split()
        status = int(parts[0])
        time_total = float(parts[1])
        size_download = int(parts[2])
        return status, time_total, size_download


# --- Prometheus histogram parsing ------------------------------------------

def scrape_histogram(metrics_url: str) -> dict[tuple[str, str, str], dict]:
    """
    Returns `{(method, endpoint, status): {"buckets": [(le, count)], "sum":, "count":}}`
    extracted from `axum_http_requests_duration_seconds`.
    """
    req = urllib.request.Request(metrics_url.rstrip("/") + "/metrics")
    with urllib.request.urlopen(req, timeout=10) as response:
        body = response.read().decode()

    out: dict[tuple[str, str, str], dict] = {}
    for line in body.splitlines():
        if not line.startswith("axum_http_requests_duration_seconds"):
            continue
        if line.startswith("#"):
            continue
        # Parse: metric{labels} value
        brace_open = line.find("{")
        brace_close = line.find("}", brace_open)
        if brace_open < 0 or brace_close < 0:
            continue
        metric_name = line[:brace_open]
        label_block = line[brace_open + 1:brace_close]
        value = float(line[brace_close + 1:].strip())
        labels = {}
        for kv in label_block.split(","):
            if "=" not in kv:
                continue
            k, v = kv.split("=", 1)
            labels[k.strip()] = v.strip().strip('"')
        method = labels.get("method", "")
        endpoint = labels.get("endpoint", "")
        status = labels.get("status", "")
        key = (method, endpoint, status)
        entry = out.setdefault(key, {"buckets": [], "sum": 0.0, "count": 0})
        if metric_name.endswith("_bucket"):
            le = labels.get("le", "")
            try:
                le_val = float("inf") if le == "+Inf" else float(le)
            except ValueError:
                continue
            entry["buckets"].append((le_val, value))
        elif metric_name.endswith("_sum"):
            entry["sum"] = value
        elif metric_name.endswith("_count"):
            entry["count"] = int(value)
    for entry in out.values():
        entry["buckets"].sort()
    return out


def histogram_quantile(entry: dict, quantile: float) -> float | None:
    """
    Linear interpolation across cumulative histogram buckets — same approach
    Prometheus uses for `histogram_quantile()`. Returns None if the entry is
    empty.
    """
    if entry["count"] == 0 or not entry["buckets"]:
        return None
    target = quantile * entry["count"]
    prev_count = 0.0
    prev_le = 0.0
    for le, count in entry["buckets"]:
        if count >= target:
            if le == float("inf"):
                # Upper bound unbounded — fall back to the previous finite
                # bucket + a small epsilon.
                return prev_le
            if count == prev_count:
                return le
            fraction = (target - prev_count) / (count - prev_count)
            return prev_le + (le - prev_le) * fraction
        prev_count = count
        prev_le = le
    return prev_le


# --- Probe runner ----------------------------------------------------------

@dataclass
class ProbeResult:
    label: str
    matched_path: str
    uri: str
    runs: list[tuple[int, float, int]] = field(default_factory=list)
    p50_ms: float | None = None
    p95_ms: float | None = None
    avg_bytes: int = 0
    status_label: str = ""

    def client_times_ms(self) -> list[float]:
        return [time_total * 1000.0 for (_, time_total, _) in self.runs]

    def client_bytes(self) -> list[int]:
        return [size for (_, _, size) in self.runs]


def classify(
    p95_ms: float | None,
    avg_bytes: int,
    ok_p95_ms: float,
    ok_bytes: int,
    slow_p95_ms: float,
    slow_bytes: int,
) -> str:
    if p95_ms is None:
        return "no-data"
    if p95_ms < ok_p95_ms and avg_bytes < ok_bytes:
        return "ok"
    if p95_ms < slow_p95_ms and avg_bytes < slow_bytes:
        return "slow"
    return "huge"


def fill_placeholders(template: str, ctx: dict[str, str]) -> str:
    return template.format(**ctx)


def resolve_probe_context(
    client: CurlClient,
    library_id: str,
) -> dict[str, str]:
    """
    Pick a concrete document_id for the document-detail probes from the list
    endpoint. Avoids hard-coding a uuid that may not exist in the target
    deployment.
    """
    uri = f"/v1/content/documents?libraryId={library_id}&limit=1"
    args = [
        "curl", "-s", "-b", client.cookie_jar,
        f"{client.base_url}{uri}",
    ]
    proc = subprocess.run(args, capture_output=True, check=False)
    if proc.returncode != 0:
        raise RuntimeError(f"context discovery failed: {proc.stderr!r}")
    payload = json.loads(proc.stdout.decode())
    items = payload.get("items") or []
    if not items:
        raise RuntimeError(
            f"library {library_id} has no documents — cannot resolve document_id"
        )
    return {
        "library_id": library_id,
        "document_id": items[0]["id"],
    }


def run_probe(
    client: CurlClient,
    probe: Probe,
    ctx: dict[str, str],
    runs: int,
) -> ProbeResult:
    uri = fill_placeholders(probe.uri_template, ctx)
    result = ProbeResult(label=probe.label, matched_path=probe.matched_path, uri=uri)
    # Warm-up hit — not recorded in `result.runs`. Rust + Postgres
    # plan cache + Arango query cache all take a plan-build hit on
    # the first execution after a deploy. Without this the p95 on
    # small --runs samples is basically the cold number and not
    # representative of steady-state latency.
    try:
        client.request(uri)
    except Exception:
        pass
    for _ in range(runs):
        try:
            result.runs.append(client.request(uri))
        except Exception as exc:
            print(f"  ! {probe.label}: {exc}", file=sys.stderr)
            result.runs.append((0, 0.0, 0))
    return result


def aggregate_client(result: ProbeResult) -> None:
    times = [t for t in result.client_times_ms() if t > 0]
    if times:
        times.sort()
        result.p50_ms = statistics.median(times)
        # p95 on small sample sizes — linear interpolation at n*0.95.
        idx = max(0, int(round(len(times) * 0.95)) - 1)
        result.p95_ms = times[idx]
    bytes_list = result.client_bytes()
    if bytes_list:
        result.avg_bytes = int(statistics.mean(bytes_list))


def merge_server_quantiles(
    result: ProbeResult,
    hist: dict[tuple[str, str, str], dict],
) -> None:
    """
    Prefer the server-side Prometheus histogram when the endpoint has
    enough samples — more accurate than client-side microsecond timing.
    Falls back silently to client times when the histogram has no data
    (e.g. handler not yet instrumented).
    """
    # Assume GET/200 — all read-only probes target happy path.
    key = ("GET", result.matched_path, "200")
    entry = hist.get(key)
    if entry is None or entry["count"] < 3:
        return
    p50 = histogram_quantile(entry, 0.50)
    p95 = histogram_quantile(entry, 0.95)
    if p50 is not None:
        result.p50_ms = p50 * 1000.0
    if p95 is not None:
        result.p95_ms = p95 * 1000.0


# --- Report writer ---------------------------------------------------------

def format_bytes(n: int) -> str:
    if n < 1024:
        return f"{n} B"
    if n < 1024 * 1024:
        return f"{n / 1024:.1f} KiB"
    return f"{n / (1024 * 1024):.2f} MiB"


def format_ms(ms: float | None) -> str:
    if ms is None:
        return "—"
    if ms < 10:
        return f"{ms:.1f} ms"
    return f"{ms:.0f} ms"


def write_report(
    out_path: pathlib.Path,
    results: list[ProbeResult],
    ctx: dict[str, str],
    base_url: str,
    runs: int,
) -> None:
    lines: list[str] = []
    lines.append("# UI endpoint profile")
    lines.append("")
    lines.append(f"- Captured at: `{datetime.now(timezone.utc).isoformat()}`")
    lines.append(f"- Base URL: `{base_url}`")
    lines.append(f"- Library: `{ctx['library_id']}`")
    lines.append(f"- Runs per probe: {runs}")
    lines.append("")
    lines.append("| Status | Probe | Matched path | p50 | p95 | avg bytes |")
    lines.append("|---|---|---|---|---|---|")
    for r in sorted(
        results,
        key=lambda item: ((item.p95_ms or 0.0) + item.avg_bytes / 1_000_000),
        reverse=True,
    ):
        lines.append(
            f"| `{r.status_label}` | `{r.label}` | `{r.matched_path}` | "
            f"{format_ms(r.p50_ms)} | {format_ms(r.p95_ms)} | {format_bytes(r.avg_bytes)} |"
        )
    lines.append("")
    lines.append("## Status buckets")
    lines.append("")
    counts: dict[str, int] = {}
    for r in results:
        counts[r.status_label] = counts.get(r.status_label, 0) + 1
    for status in ("ok", "slow", "huge", "no-data"):
        if counts.get(status):
            lines.append(f"- **{status}**: {counts[status]}")
    lines.append("")
    out_path.write_text("\n".join(lines) + "\n", encoding="utf-8")


# --- Entry point -----------------------------------------------------------

def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://localhost:19000")
    parser.add_argument("--metrics-url", default="http://localhost:9464")
    parser.add_argument(
        "--library-id",
        default=os.environ.get("IRONRAG_PROBE_LIBRARY_ID"),
        help="Reference library UUID to probe against (env: IRONRAG_PROBE_LIBRARY_ID)",
    )
    parser.add_argument("--login", default="admin")
    parser.add_argument("--password", default=os.environ.get("IRONRAG_PROBE_PASSWORD", "rustrag123"))
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument(
        "--out",
        default=None,
        help="Output markdown path (default: tmp/ui-endpoint-profile-<ts>.md)",
    )
    parser.add_argument("--ok-p95-ms", type=float, default=300.0)
    parser.add_argument("--ok-bytes", type=int, default=500 * 1024)
    parser.add_argument("--slow-p95-ms", type=float, default=1000.0)
    parser.add_argument("--slow-bytes", type=int, default=5 * 1024 * 1024)
    args = parser.parse_args()

    repo_root = pathlib.Path(__file__).resolve().parents[2]
    out_path = (
        pathlib.Path(args.out)
        if args.out
        else repo_root / "tmp" / f"ui-endpoint-profile-{int(time.time())}.md"
    )
    out_path.parent.mkdir(parents=True, exist_ok=True)

    client = CurlClient(args.base_url)
    print(f"-> logging in as {args.login}")
    client.login(args.login, args.password)

    print(f"-> resolving probe context for library {args.library_id}")
    ctx = resolve_probe_context(client, args.library_id)
    print(f"   document_id = {ctx['document_id']}")

    results: list[ProbeResult] = []
    for probe in PROBES:
        print(f"-> probing {probe.label}")
        result = run_probe(client, probe, ctx, args.runs)
        aggregate_client(result)
        results.append(result)

    print("-> pulling Prometheus histogram")
    try:
        hist = scrape_histogram(args.metrics_url)
    except Exception as exc:
        print(f"   ! failed to scrape metrics: {exc}", file=sys.stderr)
        hist = {}
    for result in results:
        merge_server_quantiles(result, hist)

    for result in results:
        result.status_label = classify(
            result.p95_ms,
            result.avg_bytes,
            args.ok_p95_ms,
            args.ok_bytes,
            args.slow_p95_ms,
            args.slow_bytes,
        )

    write_report(out_path, results, ctx, args.base_url, args.runs)
    print(f"-> wrote {out_path}")

    huge = [r for r in results if r.status_label == "huge"]
    slow = [r for r in results if r.status_label == "slow"]
    if huge:
        print(f"!! {len(huge)} huge endpoints:")
        for r in huge:
            print(
                f"   {r.label}  p95={format_ms(r.p95_ms)}  bytes={format_bytes(r.avg_bytes)}"
            )
    if slow:
        print(f"!  {len(slow)} slow endpoints:")
        for r in slow:
            print(
                f"   {r.label}  p95={format_ms(r.p95_ms)}  bytes={format_bytes(r.avg_bytes)}"
            )

    # Exit non-zero if any endpoint is huge — makes it safe to gate CI on.
    return 1 if huge else 0


if __name__ == "__main__":
    raise SystemExit(main())
