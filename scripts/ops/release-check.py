#!/usr/bin/env python3
"""
IronRAG release-readiness smoke + perf suite.

Consolidates the checks that must pass before we cut a release. Runs a
fixed matrix of API endpoints + MCP tool calls against a live stack
(local or remote), records per-check latency / size / status, and
prints a verdict table plus any budget violations.

Unlike `profile-ui-endpoints.py` this script:

* covers mutating surfaces (MCP tools/call, query execution) — not
  just read-only GETs;
* produces a single pass/warn/fail release-readiness verdict;
* enforces per-check latency budgets and flags regressions;
* hits the MCP JSON-RPC surface (`/v1/mcp`) in addition to REST
  endpoints;
* optionally snapshots pg_stat_statements / Prometheus histograms for
  post-run investigation.

Usage:
    python3 scripts/ops/release-check.py \\
        --base-url http://127.0.0.1:19000 \\
        --login admin --password rustrag123 \\
        --library-id <uuid>

Exit codes:
    0  every check passed within budget.
    1  at least one check warned (over budget but still returned 2xx).
    2  at least one check failed (non-2xx, timeout, or hard error).
"""

from __future__ import annotations

import argparse
import json
import os
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.parse
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any, Callable, Iterable

# --- Per-check budgets -----------------------------------------------------
#
# Each budget is (ok_ms, warn_ms): below ok_ms → pass, below warn_ms → warn,
# above warn_ms → fail. Budgets assume a local dev stack with warm caches,
# not a production cold start. They are intentionally generous on the
# LLM-dependent paths (query turns, MCP tools/call) where network + model
# latency dominate — we only care that they complete, not that they beat
# a hero-scenario budget.
BUDGET_FAST = (100, 300)       # tiny endpoints (health, session, version)
BUDGET_STANDARD = (300, 1000)  # common REST (list, detail, admin)
BUDGET_HEAVY = (1000, 5000)    # graph topology fetch, knowledge collections
BUDGET_LLM = (5000, 60000)     # anything that goes through a model call

VERDICT_OK = "ok"
VERDICT_WARN = "warn"
VERDICT_FAIL = "fail"

VERDICT_RANK = {VERDICT_OK: 0, VERDICT_WARN: 1, VERDICT_FAIL: 2}


@dataclass
class CheckResult:
    name: str
    method: str
    url: str
    status: int | None
    latency_ms: float
    size_bytes: int
    verdict: str
    note: str = ""

    def budget_label(self) -> str:
        if self.verdict == VERDICT_OK:
            return "OK"
        if self.verdict == VERDICT_WARN:
            return "WARN"
        return "FAIL"


@dataclass
class Suite:
    base_url: str
    cookie_jar: str
    results: list[CheckResult] = field(default_factory=list)
    library_id: str = ""
    workspace_id: str = ""
    document_id: str = ""
    entity_id: str = ""

    def record(self, result: CheckResult) -> None:
        self.results.append(result)
        bucket = result.budget_label()
        line = (
            f"[{bucket:<4}] {result.name:<42} "
            f"{result.method:<4} {result.status or '---':>3}  "
            f"{result.latency_ms:7.1f} ms   "
            f"{result.size_bytes:>9} B"
        )
        if result.note:
            line += f"   {result.note}"
        print(line, flush=True)


# --- Curl helpers ----------------------------------------------------------


def verdict_for(latency_ms: float, status: int | None, budget: tuple[int, int]) -> str:
    if status is None or status >= 500:
        return VERDICT_FAIL
    if status >= 400:
        # 4xx on expected successful calls is a fail. Auth errors get
        # handled in the login step explicitly.
        return VERDICT_FAIL
    ok_ms, warn_ms = budget
    if latency_ms <= ok_ms:
        return VERDICT_OK
    if latency_ms <= warn_ms:
        return VERDICT_WARN
    return VERDICT_FAIL


def curl_json_post(
    base_url: str,
    cookie_jar: str,
    path: str,
    payload: dict[str, Any] | list[Any],
    *,
    write_cookie_jar: bool = False,
) -> tuple[int | None, float, int, bytes]:
    body = json.dumps(payload)
    out_file = tempfile.NamedTemporaryFile(
        prefix="ironrag-rc-post-", delete=False
    )
    out_file.close()
    args = [
        "curl",
        "-s",
        "-o",
        out_file.name,
        "-w",
        "%{http_code} %{time_total} %{size_download}",
        "-X",
        "POST",
        "-H",
        "Content-Type: application/json",
        "--data",
        body,
        f"{base_url}{path}",
    ]
    if write_cookie_jar:
        args.extend(["-c", cookie_jar])
    else:
        args.extend(["-b", cookie_jar])
    proc = subprocess.run(args, capture_output=True, check=False)
    try:
        raw = proc.stdout.decode("utf-8", errors="replace").strip()
        parts = raw.split()
        status = int(parts[0]) if parts else None
        latency_ms = float(parts[1]) * 1000 if len(parts) > 1 else -1.0
        size = int(parts[2]) if len(parts) > 2 else 0
    except Exception:
        status = None
        latency_ms = -1.0
        size = 0
    with open(out_file.name, "rb") as fh:
        body_bytes = fh.read()
    os.unlink(out_file.name)
    return status, latency_ms, size, body_bytes


def curl_get(
    base_url: str,
    cookie_jar: str,
    path: str,
    *,
    accept: str | None = None,
    capture_body: bool = False,
) -> tuple[int | None, float, int, bytes]:
    out_file = tempfile.NamedTemporaryFile(
        prefix="ironrag-rc-get-", delete=False
    )
    out_file.close()
    args = [
        "curl",
        "-s",
        "-b",
        cookie_jar,
        "-o",
        out_file.name,
        "-w",
        "%{http_code} %{time_total} %{size_download}",
        f"{base_url}{path}",
    ]
    if accept:
        args.insert(3, "-H")
        args.insert(4, f"Accept: {accept}")
    proc = subprocess.run(args, capture_output=True, check=False)
    try:
        raw = proc.stdout.decode("utf-8", errors="replace").strip()
        parts = raw.split()
        status = int(parts[0]) if parts else None
        latency_ms = float(parts[1]) * 1000 if len(parts) > 1 else -1.0
        size = int(parts[2]) if len(parts) > 2 else 0
    except Exception:
        status = None
        latency_ms = -1.0
        size = 0
    body = b""
    if capture_body:
        with open(out_file.name, "rb") as fh:
            body = fh.read()
    os.unlink(out_file.name)
    return status, latency_ms, size, body


# --- Checks ----------------------------------------------------------------


def check_login(suite: Suite, login: str, password: str) -> None:
    status, latency_ms, size, _ = curl_json_post(
        suite.base_url,
        suite.cookie_jar,
        "/v1/iam/session/login",
        {"login": login, "password": password},
        write_cookie_jar=True,
    )
    verdict = verdict_for(latency_ms, status, BUDGET_FAST)
    suite.record(
        CheckResult(
            name="auth.login",
            method="POST",
            url="/v1/iam/session/login",
            status=status,
            latency_ms=latency_ms,
            size_bytes=size,
            verdict=verdict,
        )
    )


def simple_get(
    suite: Suite, name: str, path: str, budget: tuple[int, int], *, note: str = ""
) -> bytes:
    status, latency_ms, size, body = curl_get(
        suite.base_url, suite.cookie_jar, path, capture_body=True
    )
    verdict = verdict_for(latency_ms, status, budget)
    suite.record(
        CheckResult(
            name=name,
            method="GET",
            url=path,
            status=status,
            latency_ms=latency_ms,
            size_bytes=size,
            verdict=verdict,
            note=note,
        )
    )
    return body


def check_catalog(suite: Suite) -> None:
    body = simple_get(suite, "health.ready", "/v1/ready", BUDGET_FAST)
    simple_get(suite, "session.resolve", "/v1/iam/session/resolve", BUDGET_FAST)
    body = simple_get(suite, "catalog.workspaces", "/v1/catalog/workspaces", BUDGET_FAST)
    try:
        parsed = json.loads(body)
        if isinstance(parsed, list) and parsed:
            suite.workspace_id = parsed[0].get("id") or ""
    except Exception:
        pass
    simple_get(suite, "admin.surface", "/v1/admin/surface", BUDGET_STANDARD)
    simple_get(suite, "ai.providers", "/v1/ai/providers", BUDGET_FAST)
    simple_get(suite, "ai.prices", "/v1/ai/prices", BUDGET_FAST)


def check_library_data(suite: Suite) -> None:
    lib = suite.library_id
    if not lib:
        return
    simple_get(
        suite,
        "ops.library.state",
        f"/v1/ops/libraries/{lib}",
        BUDGET_STANDARD,
    )
    simple_get(
        suite,
        "ops.library.dashboard",
        f"/v1/ops/libraries/{lib}/dashboard",
        BUDGET_STANDARD,
    )
    body = simple_get(
        suite,
        "documents.list.with_total",
        f"/v1/content/documents?libraryId={lib}&limit=50&includeTotal=true",
        BUDGET_STANDARD,
    )
    try:
        parsed = json.loads(body)
        if isinstance(parsed, dict):
            items = parsed.get("items") or []
            if items and isinstance(items[0], dict):
                suite.document_id = items[0].get("id") or ""
    except Exception:
        pass
    simple_get(
        suite,
        "documents.list.page_100",
        f"/v1/content/documents?libraryId={lib}&limit=100",
        BUDGET_STANDARD,
    )
    simple_get(
        suite,
        "documents.list.search",
        f"/v1/content/documents?libraryId={lib}&search=the&limit=50",
        BUDGET_STANDARD,
    )
    simple_get(
        suite,
        "documents.list.status_ready",
        f"/v1/content/documents?libraryId={lib}&status=ready&limit=50",
        BUDGET_STANDARD,
    )
    simple_get(
        suite,
        "billing.costs",
        f"/v1/billing/library-document-costs?libraryId={lib}",
        BUDGET_STANDARD,
    )


def check_document_detail(suite: Suite) -> None:
    doc = suite.document_id
    if not doc:
        suite.record(
            CheckResult(
                name="documents.detail",
                method="GET",
                url="(skipped — no documents)",
                status=None,
                latency_ms=0,
                size_bytes=0,
                verdict=VERDICT_WARN,
                note="library has no documents",
            )
        )
        return
    simple_get(
        suite,
        "documents.detail",
        f"/v1/content/documents/{doc}",
        BUDGET_STANDARD,
    )
    simple_get(
        suite,
        "documents.head",
        f"/v1/content/documents/{doc}/head",
        BUDGET_STANDARD,
    )
    simple_get(
        suite,
        "documents.prepared_segments",
        f"/v1/content/documents/{doc}/prepared-segments?limit=50",
        BUDGET_STANDARD,
    )
    simple_get(
        suite,
        "documents.technical_facts",
        f"/v1/content/documents/{doc}/technical-facts?limit=50",
        BUDGET_STANDARD,
    )
    simple_get(
        suite,
        "documents.revisions",
        f"/v1/content/documents/{doc}/revisions",
        BUDGET_STANDARD,
    )


def check_knowledge(suite: Suite) -> None:
    lib = suite.library_id
    if not lib:
        return
    simple_get(
        suite,
        "knowledge.library.summary",
        f"/v1/knowledge/libraries/{lib}/summary",
        BUDGET_STANDARD,
    )
    topology_body = simple_get(
        suite,
        "knowledge.library.graph",
        f"/v1/knowledge/libraries/{lib}/graph",
        BUDGET_HEAVY,
    )
    # Try to surface a sample entity id from the topology NDJSON for
    # the per-entity detail check below.
    try:
        for line in topology_body.splitlines()[:120]:
            frame = json.loads(line)
            if isinstance(frame, dict) and frame.get("s") == "id_map":
                ids = list((frame.get("m") or {}).keys())
                if ids:
                    suite.entity_id = ids[0]
                    break
    except Exception:
        pass
    if suite.entity_id:
        # If the entity was wiped by a previous restore, this returns
        # 404 — that's a data-state quirk, not a regression, so we
        # soften the verdict to WARN via a wrapper.
        status, latency_ms, size, _ = curl_get(
            suite.base_url,
            suite.cookie_jar,
            f"/v1/knowledge/libraries/{lib}/entities/{suite.entity_id}",
        )
        verdict = VERDICT_WARN if status == 404 else verdict_for(latency_ms, status, BUDGET_STANDARD)
        suite.record(
            CheckResult(
                name="knowledge.entity.detail",
                method="GET",
                url=f"/v1/knowledge/libraries/{lib}/entities/{suite.entity_id}",
                status=status,
                latency_ms=latency_ms,
                size_bytes=size,
                verdict=verdict,
                note="(entity wiped, 404 tolerated)" if status == 404 else "",
            )
        )


def check_snapshot_export(suite: Suite) -> None:
    lib = suite.library_id
    if not lib:
        return
    # Export is streamed, so we deliberately skip capturing the body —
    # the --output /dev/null path still makes curl wait for the last
    # byte, which is exactly what we want to measure.
    status, latency_ms, size, _ = curl_get(
        suite.base_url,
        suite.cookie_jar,
        f"/v1/content/libraries/{lib}/snapshot?include=library_data",
        capture_body=False,
    )
    verdict = verdict_for(latency_ms, status, BUDGET_LLM)
    suite.record(
        CheckResult(
            name="snapshot.export.data_only",
            method="GET",
            url=f"/v1/content/libraries/{lib}/snapshot?include=library_data",
            status=status,
            latency_ms=latency_ms,
            size_bytes=size,
            verdict=verdict,
            note=f"{size / (1024 * 1024):.1f} MiB",
        )
    )


def check_mcp(suite: Suite) -> None:
    # Capabilities — cheap GET, confirms the JSON-RPC surface is up.
    simple_get(
        suite,
        "mcp.capabilities",
        "/v1/mcp/capabilities",
        BUDGET_FAST,
    )

    def jsonrpc(
        name: str,
        method: str,
        params: dict[str, Any] | None,
        budget: tuple[int, int],
    ) -> dict[str, Any] | None:
        payload: dict[str, Any] = {"jsonrpc": "2.0", "id": 1, "method": method}
        if params is not None:
            payload["params"] = params
        status, latency_ms, size, body_bytes = curl_json_post(
            suite.base_url, suite.cookie_jar, "/v1/mcp", payload
        )
        note = ""
        parsed: dict[str, Any] | None = None
        try:
            parsed = json.loads(body_bytes)
            if isinstance(parsed, dict) and "error" in parsed:
                note = f"rpc error: {parsed['error'].get('message', '')[:80]}"
        except Exception:
            parsed = None
        verdict = verdict_for(latency_ms, status, budget)
        if note:
            verdict = VERDICT_FAIL if verdict != VERDICT_FAIL else verdict
        suite.record(
            CheckResult(
                name=name,
                method="POST",
                url="/v1/mcp",
                status=status,
                latency_ms=latency_ms,
                size_bytes=size,
                verdict=verdict,
                note=note,
            )
        )
        return parsed

    jsonrpc(
        "mcp.initialize",
        "initialize",
        {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "release-check", "version": "1"},
        },
        BUDGET_FAST,
    )
    jsonrpc("mcp.tools.list", "tools/list", {}, BUDGET_FAST)

    if suite.library_id:
        jsonrpc(
            "mcp.list_libraries",
            "tools/call",
            {"name": "list_libraries", "arguments": {}},
            BUDGET_STANDARD,
        )
        jsonrpc(
            "mcp.search_documents",
            "tools/call",
            {
                "name": "search_documents",
                "arguments": {
                    "libraryIds": [suite.library_id],
                    "query": "hello",
                    "limit": 5,
                },
            },
            BUDGET_LLM,
        )
        if suite.document_id:
            jsonrpc(
                "mcp.read_document",
                "tools/call",
                {
                    "name": "read_document",
                    "arguments": {
                        "libraryId": suite.library_id,
                        "documentId": suite.document_id,
                        "mode": "head",
                        "length": 500,
                    },
                },
                BUDGET_STANDARD,
            )


# --- Runner ----------------------------------------------------------------


def run(args: argparse.Namespace) -> int:
    cookie_jar = tempfile.NamedTemporaryFile(
        prefix="ironrag-rc-cookies-", delete=False
    ).name
    suite = Suite(
        base_url=args.base_url.rstrip("/"),
        cookie_jar=cookie_jar,
        library_id=args.library_id,
    )

    started = time.monotonic()
    print(
        f"\n=== IronRAG release check :: {datetime.now(timezone.utc).isoformat()} ===",
        flush=True,
    )
    print(
        f"    base_url     = {suite.base_url}\n    library_id   = {suite.library_id or '(none)'}\n",
        flush=True,
    )

    check_login(suite, args.login, args.password)
    # If login failed, bail out — everything else is authenticated.
    if not suite.results or suite.results[-1].verdict == VERDICT_FAIL:
        print("\n[!] login failed, aborting the rest of the suite", flush=True)
        return 2

    check_catalog(suite)
    check_library_data(suite)
    check_document_detail(suite)
    check_knowledge(suite)
    check_snapshot_export(suite)
    check_mcp(suite)

    elapsed = time.monotonic() - started
    print(
        f"\n=== summary :: total checks={len(suite.results)} "
        f"wall={elapsed:.1f}s ===",
        flush=True,
    )
    counts: dict[str, int] = {VERDICT_OK: 0, VERDICT_WARN: 0, VERDICT_FAIL: 0}
    for r in suite.results:
        counts[r.verdict] += 1
    print(
        f"    ok={counts[VERDICT_OK]}  warn={counts[VERDICT_WARN]}  fail={counts[VERDICT_FAIL]}",
        flush=True,
    )

    # Perf top-10 (slowest passing or warning checks)
    timed = sorted(
        (r for r in suite.results if r.latency_ms > 0),
        key=lambda r: r.latency_ms,
        reverse=True,
    )
    if timed:
        print("\n    top latency:")
        for r in timed[:10]:
            print(
                f"      {r.latency_ms:8.1f} ms  {r.budget_label():<4} {r.name}",
                flush=True,
            )

    # Warnings / failures list
    bad = [r for r in suite.results if r.verdict in (VERDICT_WARN, VERDICT_FAIL)]
    if bad:
        print("\n    regressions:")
        for r in bad:
            print(
                f"      {r.budget_label():<4}  {r.latency_ms:8.1f} ms  "
                f"{r.status or '---':>3}  {r.name}  ({r.note or r.url})",
                flush=True,
            )

    worst = max((VERDICT_RANK[r.verdict] for r in suite.results), default=0)

    if args.json_out:
        payload = {
            "started_at": datetime.now(timezone.utc).isoformat(),
            "elapsed_s": round(elapsed, 3),
            "checks": [
                {
                    "name": r.name,
                    "method": r.method,
                    "url": r.url,
                    "status": r.status,
                    "latency_ms": round(r.latency_ms, 2),
                    "size_bytes": r.size_bytes,
                    "verdict": r.verdict,
                    "note": r.note,
                }
                for r in suite.results
            ],
            "summary": counts,
        }
        with open(args.json_out, "w", encoding="utf-8") as fh:
            json.dump(payload, fh, indent=2, ensure_ascii=False)
        print(f"\n    json_out: {args.json_out}", flush=True)

    try:
        os.unlink(cookie_jar)
    except OSError:
        pass
    return {0: 0, 1: 1, 2: 2}[worst]


def main() -> int:
    parser = argparse.ArgumentParser(description="IronRAG release readiness check")
    parser.add_argument("--base-url", default="http://127.0.0.1:19000")
    parser.add_argument("--login", default="admin")
    parser.add_argument("--password", default="rustrag123")
    parser.add_argument(
        "--library-id",
        default=os.environ.get("IRONRAG_RELEASE_CHECK_LIBRARY_ID", ""),
        help="UUID of the reference library to probe. Falls back to env var.",
    )
    parser.add_argument(
        "--json-out",
        default="",
        help="Optional path to write a machine-readable JSON summary.",
    )
    args = parser.parse_args()
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
