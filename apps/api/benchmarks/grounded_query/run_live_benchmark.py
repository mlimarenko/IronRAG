#!/usr/bin/env python3

from __future__ import annotations

import argparse
import hashlib
import json
import math
import mimetypes
import os
import platform
import re
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Mapping, Sequence

import requests


DEFAULT_POLL_INTERVAL_SECONDS = 5.0
DEFAULT_WAIT_TIMEOUT_SECONDS = 900.0
DEFAULT_QUERY_TOP_K = 8
RANK_METRIC_CUTOFFS = (1, 3, 5, 10)
RANK_METRIC_SEARCH_LIMIT = max(RANK_METRIC_CUTOFFS)
RANK_RELEVANCE_FILE_NAME = "rank_relevance.json"
RANK_TREND_FILE_NAME = "rank_metrics_trend.jsonl"
ANSWER_LATENCY_PERCENTILES = (50, 95, 99)
BENCHMARK_INTEGRITY_SCHEMA_VERSION = 3
HOST_SNAPSHOT_SCHEMA_VERSION = 1
ENVIRONMENT_IDENTITY_SCHEMA_VERSION = 1
CACHE_POLICIES = ("cold", "warm", "mixed")
CASE_ORDER_POLICY = "sha256_round_suite_case_v1"
ARTIFACT_DIGEST_PATTERN = re.compile(r"^(?:sha256:)?[0-9a-f]{64}$")
DEFAULT_MIN_HOST_AVAILABLE_MEMORY_PERCENT = 10.0
DEFAULT_MAX_HOST_SWAP_USED_PERCENT = 10.0
DEFAULT_MAX_HOST_CPU_PSI_SOME_AVG10 = 20.0
DEFAULT_MAX_HOST_MEMORY_PSI_SOME_AVG10 = 1.0
DEFAULT_MAX_HOST_MEMORY_PSI_FULL_AVG10 = 0.5
DEFAULT_MAX_HOST_IO_PSI_SOME_AVG10 = 5.0
DEFAULT_MAX_HOST_IO_PSI_FULL_AVG10 = 1.0
DEFAULT_SUITE_MATRIX = [
    "api_baseline_suite.json",
    "workflow_strict_suite.json",
    "layout_noise_suite.json",
    "graph_multihop_suite.json",
    "multiformat_surface_suite.json",
]


def utc_now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def lower_text(value: str | None) -> str:
    return (value or "").casefold()


def canonical_match_text(value: str | None) -> str:
    normalized = lower_text(value)
    normalized = re.sub(r"[\u2010-\u2015\u2212]+", " ", normalized)
    normalized = re.sub(r"\s+", " ", normalized)
    return normalized.strip()


def contains_all(haystack: str | None, needles: list[str]) -> bool:
    normalized = canonical_match_text(haystack)
    return all(canonical_match_text(needle) in normalized for needle in needles)


def contains_any(haystack: str | None, needles: list[str]) -> bool:
    normalized = canonical_match_text(haystack)
    return any(canonical_match_text(needle) in normalized for needle in needles)


def first_answer_sentence(answer: str | None) -> str:
    text = (answer or "").strip()
    if not text:
        return ""
    first_line = next((line.strip() for line in text.splitlines() if line.strip()), "")
    match = re.search(r"(?<=[.!?。！？])\s+", first_line)
    if match:
        return first_line[: match.start()].strip()
    return first_line[:240].strip()


def references_contain_all_labels(references: list[dict[str, Any]], labels: list[str]) -> bool:
    if not labels:
        return True
    reference_text = "\n".join(
        str(reference.get("label") or reference.get("entityType") or "")
        for reference in references
    )
    return contains_all(reference_text, labels)


def relation_references_contain_all_text(
    references: list[dict[str, Any]], expected_text: list[str]
) -> bool:
    if not expected_text:
        return True
    reference_text = "\n".join(
        "\n".join(
            str(reference.get(field) or "")
            for field in ("predicate", "normalizedAssertion", "normalized_assertion")
        )
        for reference in references
    )
    return contains_all(reference_text, expected_text)


def append_failure_if(condition: bool, failures: list[str], failure_code: str) -> None:
    if condition:
        failures.append(failure_code)


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run live grounded QA benchmarks against a IronRAG deployment."
    )
    parser.add_argument(
        "--base-url",
        default=os.environ.get("IRONRAG_BENCHMARK_BASE_URL"),
        help="IronRAG API base URL including /v1.",
    )
    parser.add_argument(
        "--suite",
        action="append",
        help="Path to one benchmark suite JSON. Can be provided multiple times.",
    )
    parser.add_argument(
        "--workspace-id",
        default=os.environ.get("IRONRAG_BENCHMARK_WORKSPACE_ID"),
        help="Workspace UUID where the benchmark library should live.",
    )
    parser.add_argument(
        "--library-id",
        help="Reuse an existing library instead of creating a fresh one.",
    )
    parser.add_argument(
        "--library-name",
        help="Display name for a freshly created library.",
    )
    parser.add_argument(
        "--wait-timeout-seconds",
        type=float,
        default=DEFAULT_WAIT_TIMEOUT_SECONDS,
        help="Maximum time to wait for readiness / quiet pipeline.",
    )
    parser.add_argument(
        "--poll-interval-seconds",
        type=float,
        default=DEFAULT_POLL_INTERVAL_SECONDS,
        help="Polling interval for ops state.",
    )
    parser.add_argument(
        "--query-top-k",
        type=int,
        default=DEFAULT_QUERY_TOP_K,
        help="topK value for grounded answer requests.",
    )
    parser.add_argument(
        "--output",
        help="Optional path to write the final matrix JSON.",
    )
    parser.add_argument(
        "--output-dir",
        help="Optional directory to write one JSON file per suite plus matrix.result.json.",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help="Exit non-zero if any strict-blocking suite fails.",
    )
    parser.add_argument(
        "--skip-upload",
        action="store_true",
        help="Reuse an existing corpus in --library-id and skip document uploads.",
    )
    parser.add_argument(
        "--canonicalize-reused-library",
        action="store_true",
        help="Wait until a reused library becomes quiet and query-ready before benchmarking.",
    )
    parser.add_argument(
        "--upload-only",
        action="store_true",
        help="Create or reuse a library, upload corpus documents, wait for readiness, print summary JSON, and exit without running QA cases.",
    )
    parser.add_argument(
        "--runtime-label",
        default=os.environ.get("IRONRAG_BENCHMARK_RUNTIME_LABEL"),
        help="Human-readable baseline/candidate label recorded in the artifact.",
    )
    parser.add_argument(
        "--runtime-artifact-digest",
        default=os.environ.get("IRONRAG_BENCHMARK_RUNTIME_ARTIFACT_DIGEST"),
        help="Required immutable image or build SHA-256 digest recorded for auditability.",
    )
    parser.add_argument(
        "--cache-policy",
        choices=CACHE_POLICIES,
        default=os.environ.get("IRONRAG_BENCHMARK_CACHE_POLICY", "cold"),
        help="Cache preparation policy applied by the benchmark orchestrator.",
    )
    parser.add_argument(
        "--round-id",
        default=os.environ.get("IRONRAG_BENCHMARK_ROUND_ID", "round-1"),
        help="Paired alternating round identifier recorded in the artifact.",
    )
    parser.add_argument(
        "--max-host-load-per-cpu",
        type=float,
        default=1.25,
        help="Refuse to start when one-minute load divided by CPU count exceeds this value.",
    )
    parser.add_argument(
        "--min-host-available-memory-percent",
        type=float,
        default=DEFAULT_MIN_HOST_AVAILABLE_MEMORY_PERCENT,
        help="Minimum MemAvailable/MemTotal percentage for release evidence.",
    )
    parser.add_argument(
        "--max-host-swap-used-percent",
        type=float,
        default=DEFAULT_MAX_HOST_SWAP_USED_PERCENT,
        help="Maximum used swap percentage for release evidence.",
    )
    parser.add_argument(
        "--max-host-cpu-psi-some-avg10",
        type=float,
        default=DEFAULT_MAX_HOST_CPU_PSI_SOME_AVG10,
        help="Maximum CPU PSI some.avg10 percentage for release evidence.",
    )
    parser.add_argument(
        "--max-host-memory-psi-some-avg10",
        type=float,
        default=DEFAULT_MAX_HOST_MEMORY_PSI_SOME_AVG10,
        help="Maximum memory PSI some.avg10 percentage for release evidence.",
    )
    parser.add_argument(
        "--max-host-memory-psi-full-avg10",
        type=float,
        default=DEFAULT_MAX_HOST_MEMORY_PSI_FULL_AVG10,
        help="Maximum memory PSI full.avg10 percentage for release evidence.",
    )
    parser.add_argument(
        "--max-host-io-psi-some-avg10",
        type=float,
        default=DEFAULT_MAX_HOST_IO_PSI_SOME_AVG10,
        help="Maximum I/O PSI some.avg10 percentage for release evidence.",
    )
    parser.add_argument(
        "--max-host-io-psi-full-avg10",
        type=float,
        default=DEFAULT_MAX_HOST_IO_PSI_FULL_AVG10,
        help="Maximum I/O PSI full.avg10 percentage for release evidence.",
    )
    parser.add_argument(
        "--allow-busy-host",
        action="store_true",
        help="Allow a diagnostic run on a busy host; artifacts remain marked non-release.",
    )
    return parser.parse_args(argv)


@dataclass
class BenchmarkCase:
    case_id: str
    question: str
    search_query: str
    expected_documents_contains: list[str]
    search_required_all: list[str]
    answer_required_all: list[str]
    answer_required_any: list[str]
    answer_opening_required_any: list[str]
    answer_forbidden_any: list[str]
    min_chunk_reference_count: int
    min_prepared_segment_reference_count: int
    min_technical_fact_reference_count: int
    min_entity_reference_count: int
    min_relation_reference_count: int
    expected_entity_reference_labels_contains: list[str]
    expected_relation_reference_text_contains: list[str]
    allowed_verification_states: list[str]
    relevant_documents: list[str]
    relevant_chunks: list[str]


def canonical_json_sha256(payload: Any) -> str:
    encoded = json.dumps(
        payload,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def _finite_number(value: Any, *, minimum: float = 0.0) -> float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    number = float(value)
    if not math.isfinite(number) or number < minimum:
        return None
    return number


def _read_linux_pressure(resource: str) -> dict[str, dict[str, float | int]]:
    pressure: dict[str, dict[str, float | int]] = {}
    path = Path("/proc/pressure") / resource
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError:
        return pressure
    for line in lines:
        tokens = line.split()
        if not tokens:
            continue
        category = tokens[0]
        values: dict[str, float | int] = {}
        for token in tokens[1:]:
            key, separator, raw_value = token.partition("=")
            if not separator:
                continue
            try:
                values[key] = int(raw_value) if key == "total" else float(raw_value)
            except ValueError:
                continue
        pressure[category] = values
    return pressure


def capture_host_snapshot() -> dict[str, Any]:
    cpu_count = os.cpu_count() or 1
    try:
        load_1m, load_5m, load_15m = os.getloadavg()
    except OSError:
        load_1m = load_5m = load_15m = None
    memory: dict[str, int] = {}
    try:
        for line in Path("/proc/meminfo").read_text(encoding="utf-8").splitlines():
            key, _, raw_value = line.partition(":")
            first_token = raw_value.strip().split(maxsplit=1)[0]
            if key in {"MemTotal", "MemAvailable", "SwapTotal", "SwapFree"}:
                memory[f"{key}KiB"] = int(first_token)
    except (OSError, ValueError, IndexError):
        memory = {}
    return {
        "schemaVersion": HOST_SNAPSHOT_SCHEMA_VERSION,
        "capturedAt": utc_now_iso(),
        "capturedMonotonicNs": time.monotonic_ns(),
        "cpuCount": cpu_count,
        "load1m": round(load_1m, 3) if load_1m is not None else None,
        "load5m": round(load_5m, 3) if load_5m is not None else None,
        "load15m": round(load_15m, 3) if load_15m is not None else None,
        "load1mPerCpu": round(load_1m / cpu_count, 4)
        if load_1m is not None
        else None,
        "pressure": {
            resource: _read_linux_pressure(resource)
            for resource in ("cpu", "memory", "io")
        },
        **memory,
    }


def build_host_eligibility_policy(
    *,
    max_load_per_cpu: float = 1.25,
    min_available_memory_percent: float = DEFAULT_MIN_HOST_AVAILABLE_MEMORY_PERCENT,
    max_swap_used_percent: float = DEFAULT_MAX_HOST_SWAP_USED_PERCENT,
    max_cpu_psi_some_avg10: float = DEFAULT_MAX_HOST_CPU_PSI_SOME_AVG10,
    max_memory_psi_some_avg10: float = DEFAULT_MAX_HOST_MEMORY_PSI_SOME_AVG10,
    max_memory_psi_full_avg10: float = DEFAULT_MAX_HOST_MEMORY_PSI_FULL_AVG10,
    max_io_psi_some_avg10: float = DEFAULT_MAX_HOST_IO_PSI_SOME_AVG10,
    max_io_psi_full_avg10: float = DEFAULT_MAX_HOST_IO_PSI_FULL_AVG10,
) -> dict[str, Any]:
    return {
        "maxLoad1mPerCpu": max_load_per_cpu,
        "minAvailableMemoryPercent": min_available_memory_percent,
        "maxSwapUsedPercent": max_swap_used_percent,
        "maxPsiSomeAvg10": {
            "cpu": max_cpu_psi_some_avg10,
            "memory": max_memory_psi_some_avg10,
            "io": max_io_psi_some_avg10,
        },
        "maxPsiFullAvg10": {
            "memory": max_memory_psi_full_avg10,
            "io": max_io_psi_full_avg10,
        },
    }


def validate_host_eligibility_policy(policy: Mapping[str, Any]) -> list[str]:
    invalid: list[str] = []
    for key in (
        "maxLoad1mPerCpu",
        "minAvailableMemoryPercent",
        "maxSwapUsedPercent",
    ):
        value = _finite_number(policy.get(key))
        if value is None or (key == "maxLoad1mPerCpu" and value <= 0.0) or (
            key != "maxLoad1mPerCpu" and value > 100.0
        ):
            invalid.append(key)
    for category, resources in (
        ("maxPsiSomeAvg10", ("cpu", "memory", "io")),
        ("maxPsiFullAvg10", ("memory", "io")),
    ):
        thresholds = policy.get(category)
        if not isinstance(thresholds, Mapping):
            invalid.append(category)
            continue
        for resource in resources:
            value = _finite_number(thresholds.get(resource))
            if value is None or value > 100.0:
                invalid.append(f"{category}.{resource}")
    return invalid


def validate_release_host_eligibility_policy(policy: Mapping[str, Any]) -> list[str]:
    """Reject policy overrides that weaken the canonical release thresholds."""
    invalid = validate_host_eligibility_policy(policy)
    if invalid:
        return invalid
    release_policy = build_host_eligibility_policy()
    if float(policy["maxLoad1mPerCpu"]) > float(release_policy["maxLoad1mPerCpu"]):
        invalid.append("maxLoad1mPerCpu:weakened")
    if float(policy["minAvailableMemoryPercent"]) < float(
        release_policy["minAvailableMemoryPercent"]
    ):
        invalid.append("minAvailableMemoryPercent:weakened")
    if float(policy["maxSwapUsedPercent"]) > float(
        release_policy["maxSwapUsedPercent"]
    ):
        invalid.append("maxSwapUsedPercent:weakened")
    for category in ("maxPsiSomeAvg10", "maxPsiFullAvg10"):
        release_thresholds = release_policy[category]
        thresholds = policy[category]
        for resource, release_value in release_thresholds.items():
            if float(thresholds[resource]) > float(release_value):
                invalid.append(f"{category}.{resource}:weakened")
    return sorted(set(invalid))


def _evaluate_load(
    snapshot: Mapping[str, Any],
    policy: Mapping[str, Any],
    computed: dict[str, float],
) -> list[str]:
    cpu_count = _finite_number(snapshot.get("cpuCount"), minimum=1.0)
    load_1m = _finite_number(snapshot.get("load1m"))
    max_load = _finite_number(policy.get("maxLoad1mPerCpu"))
    if cpu_count is None or load_1m is None or max_load is None:
        return ["load.invalid"]
    computed["load1mPerCpu"] = round(load_1m / cpu_count, 6)
    return ["load1mPerCpu"] if computed["load1mPerCpu"] > max_load else []


def _evaluate_memory(
    snapshot: Mapping[str, Any],
    policy: Mapping[str, Any],
    computed: dict[str, float],
) -> list[str]:
    memory_total = _finite_number(snapshot.get("MemTotalKiB"), minimum=1.0)
    memory_available = _finite_number(snapshot.get("MemAvailableKiB"))
    min_memory = _finite_number(policy.get("minAvailableMemoryPercent"))
    if (
        memory_total is None
        or memory_available is None
        or memory_available > memory_total
        or min_memory is None
    ):
        return ["memory.invalid"]
    computed["availableMemoryPercent"] = round(
        memory_available / memory_total * 100.0,
        6,
    )
    return (
        ["availableMemoryPercent"]
        if computed["availableMemoryPercent"] < min_memory
        else []
    )


def _evaluate_swap(
    snapshot: Mapping[str, Any],
    policy: Mapping[str, Any],
    computed: dict[str, float],
) -> list[str]:
    swap_total = _finite_number(snapshot.get("SwapTotalKiB"))
    swap_free = _finite_number(snapshot.get("SwapFreeKiB"))
    max_swap = _finite_number(policy.get("maxSwapUsedPercent"))
    if (
        swap_total is None
        or swap_free is None
        or swap_free > swap_total
        or max_swap is None
    ):
        return ["swap.invalid"]
    computed["swapUsedPercent"] = round(
        0.0
        if swap_total <= 0.0
        else (swap_total - swap_free) / swap_total * 100.0,
        6,
    )
    return ["swapUsedPercent"] if computed["swapUsedPercent"] > max_swap else []


def _evaluate_pressure_metric(
    pressure: Mapping[str, Any],
    thresholds: Any,
    resource: str,
    pressure_class: str,
    computed: dict[str, float],
) -> list[str]:
    pressure_resource = pressure.get(resource)
    pressure_values = (
        pressure_resource.get(pressure_class)
        if isinstance(pressure_resource, Mapping)
        else None
    )
    avg10 = (
        _finite_number(pressure_values.get("avg10"))
        if isinstance(pressure_values, Mapping)
        else None
    )
    threshold = (
        _finite_number(thresholds.get(resource))
        if isinstance(thresholds, Mapping)
        else None
    )
    metric = f"{resource}Psi{pressure_class.title()}Avg10"
    if avg10 is None or threshold is None:
        return [f"{metric}.invalid"]
    computed[metric] = avg10
    return [metric] if avg10 > threshold else []


def _evaluate_pressure(
    snapshot: Mapping[str, Any],
    policy: Mapping[str, Any],
    computed: dict[str, float],
) -> list[str]:
    pressure = snapshot.get("pressure")
    if not isinstance(pressure, Mapping):
        return ["pressure.invalid"]
    violations: list[str] = []
    for category, resources in (
        ("maxPsiSomeAvg10", ("cpu", "memory", "io")),
        ("maxPsiFullAvg10", ("memory", "io")),
    ):
        pressure_class = "some" if category == "maxPsiSomeAvg10" else "full"
        for resource in resources:
            violations.extend(
                _evaluate_pressure_metric(
                    pressure,
                    policy.get(category),
                    resource,
                    pressure_class,
                    computed,
                )
            )
    return violations


def evaluate_host_snapshot(
    snapshot: Mapping[str, Any],
    policy: Mapping[str, Any],
) -> dict[str, Any]:
    """Recompute host eligibility exclusively from raw snapshot measurements."""
    violations = [f"invalidPolicy:{item}" for item in validate_host_eligibility_policy(policy)]
    computed: dict[str, float] = {}
    if snapshot.get("schemaVersion") != HOST_SNAPSHOT_SCHEMA_VERSION:
        violations.append("snapshot.schemaVersion")
    violations.extend(_evaluate_load(snapshot, policy, computed))
    violations.extend(_evaluate_memory(snapshot, policy, computed))
    violations.extend(_evaluate_swap(snapshot, policy, computed))
    violations.extend(_evaluate_pressure(snapshot, policy, computed))
    return {
        "passed": not violations,
        "violations": sorted(set(violations)),
        "computed": computed,
    }


def _sequence_snapshot_values(
    phase: str,
    snapshot: Mapping[str, Any] | None,
) -> tuple[datetime | None, int | None, list[str]]:
    violations: list[str] = []
    captured_at = snapshot.get("capturedAt") if snapshot is not None else None
    try:
        timestamp = (
            datetime.fromisoformat(captured_at)
            if isinstance(captured_at, str)
            else None
        )
    except ValueError:
        timestamp = None
    if timestamp is None or timestamp.tzinfo is None:
        violations.append(f"{phase}.capturedAt")
        timestamp = None
    monotonic_value = (
        snapshot.get("capturedMonotonicNs") if snapshot is not None else None
    )
    if (
        isinstance(monotonic_value, bool)
        or not isinstance(monotonic_value, int)
        or monotonic_value < 0
    ):
        violations.append(f"{phase}.capturedMonotonicNs")
        monotonic_value = None
    return timestamp, monotonic_value, violations


def _strictly_increasing(values: Mapping[str, Any]) -> bool:
    return (
        len(values) == 3
        and values["pre"] < values["mid"] < values["post"]
    )


def evaluate_host_snapshot_sequence(
    snapshots: Mapping[str, Mapping[str, Any]],
) -> dict[str, Any]:
    """Require independently captured, chronologically ordered pre/mid/post samples."""
    violations: list[str] = []
    timestamps: dict[str, datetime] = {}
    monotonic_values: dict[str, int] = {}
    for phase in ("pre", "mid", "post"):
        snapshot = snapshots.get(phase)
        timestamp, monotonic_value, phase_violations = _sequence_snapshot_values(
            phase,
            snapshot if isinstance(snapshot, Mapping) else None,
        )
        violations.extend(phase_violations)
        if timestamp is not None:
            timestamps[phase] = timestamp
        if monotonic_value is not None:
            monotonic_values[phase] = monotonic_value
    if len(timestamps) == 3 and not _strictly_increasing(timestamps):
        violations.append("capturedAt:notStrictlyIncreasing")
    if len(monotonic_values) == 3 and not _strictly_increasing(monotonic_values):
        violations.append("capturedMonotonicNs:notStrictlyIncreasing")
    return {"passed": not violations, "violations": violations}


def valid_artifact_digest(value: Any) -> bool:
    return isinstance(value, str) and bool(ARTIFACT_DIGEST_PATTERN.fullmatch(value.strip()))


def normalized_artifact_digest(value: str) -> str:
    normalized = value.strip()
    return normalized.removeprefix("sha256:")


def _sha256_file_value(path: Path) -> str | None:
    try:
        value = path.read_text(encoding="utf-8").strip()
    except OSError:
        return None
    return hashlib.sha256(value.encode("utf-8")).hexdigest() if value else None


def _sha256_first_file_value(paths: Sequence[Path]) -> str | None:
    return next(
        (value for path in paths if (value := _sha256_file_value(path)) is not None),
        None,
    )


def _read_cgroup_value(path: str) -> str:
    try:
        return Path(path).read_text(encoding="utf-8").strip()
    except OSError:
        return "unavailable"


def _read_cgroup_cpu_limit() -> str:
    unified = _read_cgroup_value("/sys/fs/cgroup/cpu.max")
    if unified != "unavailable":
        return unified
    quota = _read_cgroup_value("/sys/fs/cgroup/cpu/cpu.cfs_quota_us")
    period = _read_cgroup_value("/sys/fs/cgroup/cpu/cpu.cfs_period_us")
    return (
        f"{quota} {period}"
        if "unavailable" not in (quota, period)
        else "unavailable"
    )


def _read_cgroup_memory_limit() -> str:
    unified = _read_cgroup_value("/sys/fs/cgroup/memory.max")
    return (
        unified
        if unified != "unavailable"
        else _read_cgroup_value("/sys/fs/cgroup/memory/memory.limit_in_bytes")
    )


def build_environment_identity(
    host_snapshot: Mapping[str, Any],
    *,
    system: str | None = None,
    kernel_release: str | None = None,
    machine: str | None = None,
    host_id_sha256: str | None = None,
    boot_id_sha256: str | None = None,
    cpu_affinity_count: int | None = None,
    cgroup_cpu_max: str | None = None,
    cgroup_memory_max: str | None = None,
) -> dict[str, Any]:
    if cpu_affinity_count is None:
        try:
            cpu_affinity_count = len(os.sched_getaffinity(0))
        except (AttributeError, OSError):
            cpu_affinity_count = os.cpu_count() or 1
    definition = {
        "schemaVersion": ENVIRONMENT_IDENTITY_SCHEMA_VERSION,
        "system": system or platform.system(),
        "kernelRelease": kernel_release or platform.release(),
        "machine": machine or platform.machine(),
        "hostIdSha256": host_id_sha256
        or _sha256_first_file_value(
            (
                Path("/etc/machine-id"),
                Path("/var/lib/dbus/machine-id"),
                Path("/sys/class/dmi/id/product_uuid"),
            )
        ),
        "bootIdSha256": boot_id_sha256
        or _sha256_file_value(Path("/proc/sys/kernel/random/boot_id")),
        "cpuCount": host_snapshot.get("cpuCount"),
        "cpuAffinityCount": cpu_affinity_count,
        "memTotalKiB": host_snapshot.get("MemTotalKiB"),
        "cgroupCpuMax": cgroup_cpu_max
        if cgroup_cpu_max is not None
        else _read_cgroup_cpu_limit(),
        "cgroupMemoryMax": cgroup_memory_max
        if cgroup_memory_max is not None
        else _read_cgroup_memory_limit(),
    }
    return {**definition, "fingerprintSha256": canonical_json_sha256(definition)}


def _validate_identity_strings(identity: Mapping[str, Any]) -> list[str]:
    invalid: list[str] = []
    for key in (
        "system",
        "kernelRelease",
        "machine",
        "hostIdSha256",
        "bootIdSha256",
        "cgroupCpuMax",
        "cgroupMemoryMax",
    ):
        value = identity.get(key)
        is_unavailable_cgroup = (
            key in {"cgroupCpuMax", "cgroupMemoryMax"} and value == "unavailable"
        )
        if not isinstance(value, str) or not value.strip() or is_unavailable_cgroup:
            invalid.append(key)
    return invalid


def _validate_identity_fingerprint(identity: Mapping[str, Any]) -> list[str]:
    fingerprint = identity.get("fingerprintSha256")
    if not valid_sha256(fingerprint):
        return ["fingerprintSha256"]
    definition = {
        key: value for key, value in identity.items() if key != "fingerprintSha256"
    }
    return (
        ["fingerprintSha256:stale"]
        if canonical_json_sha256(definition) != fingerprint
        else []
    )


def validate_environment_identity(identity: Mapping[str, Any]) -> list[str]:
    invalid = _validate_identity_strings(identity)
    if identity.get("schemaVersion") != ENVIRONMENT_IDENTITY_SCHEMA_VERSION:
        invalid.append("schemaVersion")
    for key in ("hostIdSha256", "bootIdSha256"):
        if key not in invalid and not valid_sha256(identity.get(key)):
            invalid.append(key)
    invalid.extend(
        key
        for key in ("cpuCount", "cpuAffinityCount", "memTotalKiB")
        if _finite_number(identity.get(key), minimum=1.0) is None
    )
    invalid.extend(_validate_identity_fingerprint(identity))
    return sorted(set(invalid))


def valid_sha256(value: Any) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(character in "0123456789abcdef" for character in value)
    )


def resolve_session_cookie(environment: Mapping[str, str] | None = None) -> str:
    env = os.environ if environment is None else environment
    direct = env.get("IRONRAG_SESSION_COOKIE", "").strip()
    file_name = env.get("IRONRAG_SESSION_COOKIE_FILE", "").strip()
    if direct and file_name:
        raise ValueError(
            "set only one of IRONRAG_SESSION_COOKIE or IRONRAG_SESSION_COOKIE_FILE"
        )
    if file_name:
        try:
            direct = Path(file_name).read_text(encoding="utf-8").strip()
        except OSError as error:
            raise ValueError(f"cannot read IRONRAG_SESSION_COOKIE_FILE: {error}") from error
    if not direct:
        raise ValueError(
            "IRONRAG_SESSION_COOKIE or IRONRAG_SESSION_COOKIE_FILE is required"
        )
    return direct


def permute_cases_for_round(
    cases: Sequence[BenchmarkCase],
    round_id: str,
    suite_id: str,
) -> list[BenchmarkCase]:
    """Return a stable pseudo-random case order keyed by paired round and suite."""
    order = permute_case_ids_for_round(
        [case.case_id for case in cases],
        round_id,
        suite_id,
    )
    cases_by_id = {case.case_id: case for case in cases}
    if len(cases_by_id) != len(cases):
        raise ValueError(f"duplicate case id in suite {suite_id}")
    return [cases_by_id[case_id] for case_id in order]


def permute_case_ids_for_round(
    case_ids: Sequence[str],
    round_id: str,
    suite_id: str,
) -> list[str]:
    return sorted(
        case_ids,
        key=lambda case_id: (
            hashlib.sha256(
                f"{CASE_ORDER_POLICY}\0{round_id}\0{suite_id}\0{case_id}".encode(
                    "utf-8"
                )
            ).digest(),
            case_id,
        ),
    )


def case_definition_payload(case: BenchmarkCase) -> dict[str, Any]:
    """Return every suite-controlled input that can affect a case outcome."""
    return {
        "caseId": case.case_id,
        "question": case.question,
        "searchQuery": case.search_query,
        "expectedDocumentsContains": case.expected_documents_contains,
        "searchRequiredAll": case.search_required_all,
        "answerRequiredAll": case.answer_required_all,
        "answerRequiredAny": case.answer_required_any,
        "answerOpeningRequiredAny": case.answer_opening_required_any,
        "answerForbiddenAny": case.answer_forbidden_any,
        "minChunkReferenceCount": case.min_chunk_reference_count,
        "minPreparedSegmentReferenceCount": case.min_prepared_segment_reference_count,
        "minTechnicalFactReferenceCount": case.min_technical_fact_reference_count,
        "minEntityReferenceCount": case.min_entity_reference_count,
        "minRelationReferenceCount": case.min_relation_reference_count,
        "expectedEntityReferenceLabelsContains": (
            case.expected_entity_reference_labels_contains
        ),
        "expectedRelationReferenceTextContains": (
            case.expected_relation_reference_text_contains
        ),
        "allowedVerificationStates": case.allowed_verification_states,
        # These values include rank_relevance.json overrides resolved by
        # load_suite, so changing an external relevance label changes the
        # definition hash just like changing the suite JSON itself.
        "relevantDocuments": case.relevant_documents,
        "relevantChunks": case.relevant_chunks,
    }


def case_definition_sha256(case: BenchmarkCase) -> str:
    return canonical_json_sha256(
        {
            "schemaVersion": BENCHMARK_INTEGRITY_SCHEMA_VERSION,
            "case": case_definition_payload(case),
        }
    )


def corpus_bytes_sha256(document_bytes: list[bytes]) -> str:
    """Hash ordered corpus bytes with unambiguous length framing."""
    digest = hashlib.sha256()
    digest.update(b"ironrag-grounded-corpus-v1\0")
    digest.update(len(document_bytes).to_bytes(8, byteorder="big", signed=False))
    for index, payload in enumerate(document_bytes):
        digest.update(index.to_bytes(8, byteorder="big", signed=False))
        digest.update(len(payload).to_bytes(8, byteorder="big", signed=False))
        digest.update(payload)
    return digest.hexdigest()


def corpus_sha256(document_paths: list[Path]) -> str:
    return corpus_bytes_sha256([document_path.read_bytes() for document_path in document_paths])


def build_suite_integrity(
    suite_payload: dict[str, Any],
    cases: list[BenchmarkCase],
    document_paths: list[Path],
    server_document_bytes: dict[Path, bytes],
) -> dict[str, Any]:
    document_references = [
        Path(str(item)).as_posix() for item in suite_payload.get("documents", [])
    ]
    suite_definition = {
        "schemaVersion": BENCHMARK_INTEGRITY_SCHEMA_VERSION,
        "sessionPolicy": "isolated_per_case",
        "caseOrderPolicy": CASE_ORDER_POLICY,
        "suiteId": suite_payload.get("suiteId"),
        "strictBlocking": bool(suite_payload.get("strictBlocking", True)),
        "documents": document_references,
        "cases": [case_definition_payload(case) for case in cases],
    }
    local_corpus_sha256 = corpus_sha256(document_paths)
    server_corpus_sha256 = corpus_bytes_sha256(
        [server_document_bytes[path] for path in document_paths]
    )
    if server_corpus_sha256 != local_corpus_sha256:
        raise RuntimeError(
            f"server corpus bytes do not match local fixtures for suite {suite_payload.get('suiteId')}"
        )
    return {
        "schemaVersion": BENCHMARK_INTEGRITY_SCHEMA_VERSION,
        "suiteDefinitionSha256": canonical_json_sha256(suite_definition),
        "corpusSha256": local_corpus_sha256,
        "serverCorpusSha256": server_corpus_sha256,
        "corpusVerification": "server_source_bytes",
    }


def build_matrix_integrity(
    suite_results: list[dict[str, Any]],
    *,
    query_top_k: int,
    skip_upload: bool,
    canonicalize_reused_library: bool,
    cache_policy: str = "cold",
    round_id: str = "round-1",
) -> dict[str, Any]:
    if cache_policy not in CACHE_POLICIES:
        raise ValueError(f"unsupported cache policy: {cache_policy}")
    definition = {
        "schemaVersion": BENCHMARK_INTEGRITY_SCHEMA_VERSION,
        "sessionPolicy": "isolated_per_case",
        "caseOrderPolicy": CASE_ORDER_POLICY,
        "queryTopK": query_top_k,
        "skipUpload": skip_upload,
        "canonicalizeReusedLibrary": canonicalize_reused_library,
        "cachePolicy": cache_policy,
        "roundId": round_id,
        "answerBeforeRankProbe": True,
        "suites": [
            {
                "suiteId": (suite.get("suite") or {}).get("suiteId"),
                "suiteDefinitionSha256": (suite.get("integrity") or {}).get(
                    "suiteDefinitionSha256"
                ),
                "corpusSha256": (suite.get("integrity") or {}).get("corpusSha256"),
                "serverCorpusSha256": (suite.get("integrity") or {}).get(
                    "serverCorpusSha256"
                ),
                "corpusVerification": (suite.get("integrity") or {}).get(
                    "corpusVerification"
                ),
                "caseOrder": suite.get("caseOrder"),
            }
            for suite in suite_results
        ],
    }
    return {
        "schemaVersion": BENCHMARK_INTEGRITY_SCHEMA_VERSION,
        "sessionPolicy": "isolated_per_case",
        "caseOrderPolicy": CASE_ORDER_POLICY,
        "queryTopK": query_top_k,
        "skipUpload": skip_upload,
        "canonicalizeReusedLibrary": canonicalize_reused_library,
        "cachePolicy": cache_policy,
        "roundId": round_id,
        "answerBeforeRankProbe": True,
        "matrixDefinitionSha256": canonical_json_sha256(definition),
    }


class BenchmarkClient:
    def __init__(self, base_url: str, session_cookie: str) -> None:
        self.base_url = base_url.rstrip("/")
        self.session_cookie = session_cookie
        self.http = self._new_session()

    def _new_session(self) -> requests.Session:
        http = requests.Session()
        http.cookies.set("ironrag_ui_session", self.session_cookie, path="/")
        return http

    def _refresh_session(self) -> None:
        self.http = self._new_session()

    def get_json(self, path: str, **kwargs: Any) -> Any:
        last_error: requests.ConnectionError | None = None
        for attempt in range(3):
            try:
                response = self.http.get(f"{self.base_url}{path}", timeout=120, **kwargs)
                response.raise_for_status()
                return response.json()
            except requests.ConnectionError as error:
                last_error = error
                self._refresh_session()
                if attempt < 2:
                    time.sleep(0.5 * (attempt + 1))
        assert last_error is not None
        raise last_error

    def get_bytes(self, path: str) -> bytes:
        response = self.http.get(
            f"{self.base_url}{path}",
            timeout=120,
            allow_redirects=False,
        )
        response.raise_for_status()
        return response.content

    def post_json(self, path: str, payload: dict[str, Any], **kwargs: Any) -> Any:
        response = self.http.post(
            f"{self.base_url}{path}",
            json=payload,
            timeout=300,
            **kwargs,
        )
        response.raise_for_status()
        return response.json()

    def post_multipart(self, path: str, fields: dict[str, str], file_path: Path) -> Any:
        mime_type = mimetypes.guess_type(file_path.name)[0] or "application/octet-stream"
        with file_path.open("rb") as handle:
            files = {"file": (file_path.name, handle, mime_type)}
            response = self.http.post(
                f"{self.base_url}{path}",
                data=fields,
                files=files,
                timeout=300,
            )
        response.raise_for_status()
        return response.json()


def load_rank_relevance(path: Path) -> dict[str, Any]:
    relevance_path = path.parent / RANK_RELEVANCE_FILE_NAME
    if not relevance_path.exists():
        return {}
    payload = json.loads(relevance_path.read_text(encoding="utf-8"))
    return payload.get("suites", {})


def suite_case_rank_relevance(
    rank_relevance: dict[str, Any],
    suite_id: str | None,
    case_id: str,
) -> dict[str, Any]:
    if not suite_id:
        return {}
    suite_relevance = rank_relevance.get(suite_id, {})
    if not isinstance(suite_relevance, dict):
        return {}
    cases = suite_relevance.get("cases", {})
    if not isinstance(cases, dict):
        return {}
    value = cases.get(case_id, {})
    return value if isinstance(value, dict) else {}


def load_suite(path: Path) -> tuple[list[Path], list[BenchmarkCase], dict[str, Any]]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    suite_id = payload.get("suiteId")
    rank_relevance = load_rank_relevance(path)
    documents = []
    for item in payload["documents"]:
        candidate = Path(item)
        if not candidate.is_absolute():
            candidate = (path.parent / candidate).resolve()
        documents.append(candidate)

    cases = []
    for item in payload["cases"]:
        relevance = suite_case_rank_relevance(rank_relevance, suite_id, item["id"])
        cases.append(
            BenchmarkCase(
                case_id=item["id"],
                question=item["question"],
                search_query=item.get("searchQuery", item["question"]),
                expected_documents_contains=item.get("expectedDocumentsContains", []),
                search_required_all=item.get("searchRequiredAll", []),
                answer_required_all=item.get("answerRequiredAll", []),
                answer_required_any=item.get("answerRequiredAny", []),
                answer_opening_required_any=item.get("answerOpeningRequiredAny", []),
                answer_forbidden_any=item.get("answerForbiddenAny", []),
                min_chunk_reference_count=item.get("minChunkReferenceCount", 0),
                min_prepared_segment_reference_count=item.get(
                    "minPreparedSegmentReferenceCount", 0
                ),
                min_technical_fact_reference_count=item.get("minTechnicalFactReferenceCount", 0),
                min_entity_reference_count=item.get("minEntityReferenceCount", 0),
                min_relation_reference_count=item.get("minRelationReferenceCount", 0),
                expected_entity_reference_labels_contains=item.get(
                    "expectedEntityReferenceLabelsContains", []
                ),
                expected_relation_reference_text_contains=item.get(
                    "expectedRelationReferenceTextContains", []
                ),
                allowed_verification_states=item.get("allowedVerificationStates", ["verified"]),
                relevant_documents=item.get(
                    "relevantDocuments",
                    relevance.get(
                        "relevantDocuments",
                        item.get("expectedDocumentsContains", []),
                    ),
                ),
                relevant_chunks=item.get(
                    "relevantChunks",
                    relevance.get("relevantChunks", []),
                ),
            )
        )
    return documents, cases, payload


def default_suite_paths() -> list[Path]:
    base = Path(__file__).resolve().parent
    return [base / item for item in DEFAULT_SUITE_MATRIX]


def create_library(client: BenchmarkClient, workspace_id: str, library_name: str) -> dict[str, Any]:
    return client.post_json(
        f"/catalog/workspaces/{workspace_id}/libraries",
        {
            "displayName": library_name,
            "description": "Neutral benchmark corpus for grounded QA evaluation",
        },
    )


def upload_documents(
    client: BenchmarkClient,
    library_id: str,
    document_paths: list[Path],
) -> list[dict[str, Any]]:
    uploads: list[dict[str, Any]] = []
    for document_path in document_paths:
        uploads.append(
            client.post_multipart(
                "/content/documents/upload",
                {"library_id": library_id},
                document_path,
            )
        )
    return uploads


def snapshot_library_state(
    client: BenchmarkClient,
    library_id: str,
    started_monotonic: float,
) -> dict[str, Any]:
    snapshot = client.get_json(f"/ops/libraries/{library_id}")
    state = snapshot["state"]
    return {
        "elapsedSeconds": round(time.monotonic() - started_monotonic, 3),
        "queueDepth": state["queueDepth"],
        "runningAttempts": state["runningAttempts"],
        "readableDocumentCount": state["readableDocumentCount"],
        "degradedState": state["degradedState"],
        "knowledgeGenerationState": state["knowledgeGenerationState"],
        "latestKnowledgeGenerationId": state["latestKnowledgeGenerationId"],
    }


def fetch_library_summary(client: BenchmarkClient, library_id: str) -> dict[str, Any]:
    return client.get_json(f"/knowledge/libraries/{library_id}/summary")


def fetch_topology_counts(client: BenchmarkClient, library_id: str) -> dict[str, int]:
    response = client.http.get(
        f"{client.base_url}/knowledge/libraries/{library_id}/graph",
        headers={"Accept": "application/x-ndjson"},
        timeout=120,
    )
    response.raise_for_status()

    documents = 0
    entities = 0
    relations = 0
    document_links = 0
    for line in response.text.splitlines():
        payload = line.strip()
        if not payload:
            continue
        frame = json.loads(payload)
        section = frame.get("s")
        rows = frame.get("d")
        if section == "docs" and isinstance(rows, list):
            documents += len(rows)
        elif section == "nodes" and isinstance(rows, list):
            entities += len(rows)
        elif section == "edges" and isinstance(rows, list):
            relations += len(rows)
        elif section == "doc_links" and isinstance(rows, list):
            document_links += len(rows)

    return {
        "documents": documents,
        "entities": entities,
        "relations": relations,
        "documentLinks": document_links,
    }


def wait_for_library_state(
    client: BenchmarkClient,
    library_id: str,
    minimum_readable_count: int,
    poll_interval_seconds: float,
    wait_timeout_seconds: float,
) -> tuple[list[dict[str, Any]], float | None, float | None]:
    timeline: list[dict[str, Any]] = []
    started = time.monotonic()
    readable_elapsed: float | None = None

    while True:
        point = snapshot_library_state(client, library_id, started)
        timeline.append(point)

        if readable_elapsed is None and point["readableDocumentCount"] >= minimum_readable_count:
            readable_elapsed = point["elapsedSeconds"]

        if readable_elapsed is not None and point["queueDepth"] == 0 and point["runningAttempts"] == 0:
            return timeline, readable_elapsed, point["elapsedSeconds"]

        if point["elapsedSeconds"] >= wait_timeout_seconds:
            return timeline, readable_elapsed, None

        time.sleep(poll_interval_seconds)


def create_query_session(client: BenchmarkClient, workspace_id: str, library_id: str) -> dict[str, Any]:
    return client.post_json(
        "/query/sessions",
        {
            "workspaceId": workspace_id,
            "libraryId": library_id,
            "title": f"Benchmark {utc_now_iso()}",
        },
    )


def stable_key(value: str | None) -> str:
    return canonical_match_text(Path(value).stem if value else "")


def document_stable_keys(document: dict[str, Any]) -> list[str]:
    candidates = [
        document.get("file_name"),
        document.get("fileName"),
        document.get("title"),
        document.get("external_key"),
        document.get("externalKey"),
    ]
    keys: list[str] = []
    for value in candidates:
        if not value:
            continue
        text = str(value)
        for candidate in (text, Path(text).name, Path(text).stem):
            key = stable_key(candidate)
            if key and key not in keys:
                keys.append(key)
    return keys


def primary_document_stable_key(document: dict[str, Any]) -> str | None:
    keys = document_stable_keys(document)
    return keys[0] if keys else None


def _expected_documents_by_name(document_paths: list[Path]) -> dict[str, Path]:
    expected_by_name: dict[str, Path] = {}
    for path in document_paths:
        if path.name in expected_by_name:
            raise RuntimeError(f"benchmark fixture filename is ambiguous: {path.name}")
        expected_by_name[path.name] = path
    return expected_by_name


def _primary_documents_by_name(
    knowledge_documents: list[dict[str, Any]],
) -> dict[str, dict[str, Any]]:
    actual_by_name: dict[str, dict[str, Any]] = {}
    for document in knowledge_documents:
        if (
            document.get("deleted_at") is not None
            or document.get("readable_revision_id") is None
            or document.get("document_role", "primary") != "primary"
        ):
            continue
        file_name = document.get("file_name")
        if not isinstance(file_name, str) or not file_name:
            raise RuntimeError("readable benchmark document is missing file_name")
        if file_name in actual_by_name:
            raise RuntimeError(f"benchmark library has duplicate primary source: {file_name}")
        actual_by_name[file_name] = document
    return actual_by_name


def _ensure_matching_document_inventory(
    expected_by_name: Mapping[str, Path],
    actual_by_name: Mapping[str, dict[str, Any]],
) -> None:
    expected_names = set(expected_by_name)
    actual_names = set(actual_by_name)
    if actual_names == expected_names:
        return
    missing = sorted(expected_names - actual_names)
    unexpected = sorted(actual_names - expected_names)
    raise RuntimeError(
        "benchmark library corpus inventory mismatch: "
        f"missing={missing}, unexpected={unexpected}"
    )


def _verify_document_bytes(
    client: BenchmarkClient,
    file_name: str,
    path: Path,
    document: Mapping[str, Any],
) -> bytes:
    document_id = document.get("document_id")
    if not isinstance(document_id, str) or not document_id:
        raise RuntimeError(f"benchmark document {file_name} is missing document_id")
    server_bytes = client.get_bytes(f"/content/documents/{document_id}/source")
    if server_bytes != path.read_bytes():
        raise RuntimeError(f"benchmark source bytes differ from fixture: {file_name}")
    return server_bytes


def verify_server_corpus_bytes(
    client: BenchmarkClient,
    library_id: str,
    document_paths: list[Path],
) -> dict[Path, bytes]:
    """Prove that a benchmark library contains exactly the expected primary sources."""
    expected_by_name = _expected_documents_by_name(document_paths)
    knowledge_documents = client.get_json(f"/knowledge/libraries/{library_id}/documents")
    actual_by_name = _primary_documents_by_name(knowledge_documents)
    _ensure_matching_document_inventory(expected_by_name, actual_by_name)
    return {
        path: _verify_document_bytes(client, file_name, path, actual_by_name[file_name])
        for file_name, path in expected_by_name.items()
    }


def normalize_relevant_keys(values: list[str]) -> set[str]:
    keys = set()
    for value in values:
        key = stable_key(value)
        if key:
            keys.add(key)
    return keys


def compute_rank_metrics(
    ranked_ids: list[str],
    relevant_ids: set[str],
    cutoffs: tuple[int, ...] = RANK_METRIC_CUTOFFS,
) -> dict[str, Any] | None:
    relevant = {item for item in relevant_ids if item}
    if not relevant:
        return None
    ranked = [item for item in ranked_ids if item]
    first_rank = next(
        (index + 1 for index, item in enumerate(ranked) if item in relevant),
        None,
    )
    metrics: dict[str, Any] = {
        "rankedCount": len(ranked),
        "relevantCount": len(relevant),
        "firstRelevantRank": first_rank,
        "mrr": round(1.0 / first_rank, 6) if first_rank else 0.0,
    }
    for cutoff in cutoffs:
        metrics[f"hit@{cutoff}"] = any(item in relevant for item in ranked[:cutoff])
    return metrics


def compute_marker_rank_metrics(
    ranked_texts: list[str],
    relevant_markers: list[str],
    cutoffs: tuple[int, ...] = RANK_METRIC_CUTOFFS,
) -> dict[str, Any] | None:
    markers = [canonical_match_text(marker) for marker in relevant_markers if marker.strip()]
    if not markers:
        return None
    first_rank = None
    normalized_ranked = [canonical_match_text(item) for item in ranked_texts]
    for index, text in enumerate(normalized_ranked):
        if any(marker in text for marker in markers):
            first_rank = index + 1
            break
    metrics: dict[str, Any] = {
        "rankedCount": len(normalized_ranked),
        "relevantCount": len(markers),
        "firstRelevantRank": first_rank,
        "mrr": round(1.0 / first_rank, 6) if first_rank else 0.0,
    }
    for cutoff in cutoffs:
        metrics[f"hit@{cutoff}"] = any(
            any(marker in text for marker in markers) for text in normalized_ranked[:cutoff]
        )
    return metrics


def summarize_search_hits(
    search_payload: dict[str, Any],
) -> tuple[list[dict[str, Any]], str, list[str], list[str]]:
    summaries: list[dict[str, Any]] = []
    chunk_texts: list[str] = []
    ranked_document_keys: list[str] = []
    ranked_chunk_texts: list[str] = []

    for hit in search_payload.get("documentHits", []):
        document = hit.get("document", {})
        document_id = document.get("document_id") or document.get("documentId")
        file_name = document.get("file_name") or document.get("fileName")
        title = document.get("title") or file_name
        document_key = primary_document_stable_key(document)
        if document_key:
            ranked_document_keys.append(document_key)
        chunk_summaries = []
        for chunk in hit.get("chunkHits", []):
            content = (
                chunk.get("content_text")
                or chunk.get("contentText")
                or chunk.get("normalized_text")
                or chunk.get("normalizedText")
                or ""
            )
            chunk_texts.append(content)
            ranked_chunk_texts.append(content)
            chunk_summaries.append(
                {
                    "chunkId": chunk.get("chunk_id") or chunk.get("chunkId"),
                    "documentId": document_id,
                    "score": chunk.get("score") or chunk.get("lexicalScore"),
                    "contentPreview": content[:600],
                }
            )
        vector_chunk_summaries = []
        for chunk in hit.get("vectorChunkHits", []):
            vector_chunk_summaries.append(
                {
                    "chunkId": chunk.get("chunk_id") or chunk.get("chunkId"),
                    "documentId": document_id,
                    "score": chunk.get("score"),
                }
            )
        summaries.append(
            {
                "documentId": document_id,
                "fileName": file_name,
                "title": title,
                "stableKey": document_key,
                "score": hit.get("score"),
                "chunkHits": chunk_summaries,
                "vectorChunkHits": vector_chunk_summaries,
            }
        )

    return summaries, "\n".join(chunk_texts), ranked_document_keys, ranked_chunk_texts


def case_metadata(case: BenchmarkCase) -> dict[str, Any]:
    return {
        "caseId": case.case_id,
        "caseDefinitionSha256": case_definition_sha256(case),
        "question": case.question,
        "searchQuery": case.search_query,
        "relevantDocuments": case.relevant_documents,
        "relevantChunks": case.relevant_chunks,
        "minChunkReferenceCount": case.min_chunk_reference_count,
        "minPreparedSegmentReferenceCount": case.min_prepared_segment_reference_count,
        "minTechnicalFactReferenceCount": case.min_technical_fact_reference_count,
        "minEntityReferenceCount": case.min_entity_reference_count,
        "minRelationReferenceCount": case.min_relation_reference_count,
        "expectedEntityReferenceLabelsContains": case.expected_entity_reference_labels_contains,
        "expectedRelationReferenceTextContains": case.expected_relation_reference_text_contains,
        "allowedVerificationStates": case.allowed_verification_states,
    }


def run_case(
    client: BenchmarkClient,
    library_id: str,
    session_id: str,
    case: BenchmarkCase,
    query_top_k: int,
) -> dict[str, Any]:
    # The timed answer must run before the auxiliary rank probe. Otherwise the
    # search endpoint can warm Postgres/index/provider caches and make the
    # answer latency look artificially better than a real first turn.
    answer_started = time.monotonic()
    turn_payload = client.post_json(
        f"/query/sessions/{session_id}/turns",
        {"contentText": case.question, "topK": query_top_k, "includeDebug": True},
    )
    answer_latency_ms = round((time.monotonic() - answer_started) * 1000.0, 1)
    response_turn = turn_payload.get("responseTurn", {})
    execution = turn_payload.get("execution", {})
    answer_text = response_turn.get("contentText") or response_turn.get("content_text") or ""
    execution_id = execution.get("id") or execution.get("executionId")
    execution_detail = client.get_json(f"/query/executions/{execution_id}")

    search_started = time.monotonic()
    search_payload = client.get_json(
        f"/knowledge/libraries/{library_id}/search/documents",
        params={
            "query": case.search_query,
            "limit": max(RANK_METRIC_SEARCH_LIMIT, query_top_k),
            "chunkHitLimitPerDocument": RANK_METRIC_SEARCH_LIMIT,
            "evidenceSampleLimit": 0,
        },
    )
    search_latency_ms = round((time.monotonic() - search_started) * 1000.0, 1)
    search_summaries, aggregated_chunk_text, ranked_document_keys, ranked_chunk_texts = (
        summarize_search_hits(search_payload)
    )
    top_document_title = search_summaries[0]["title"] if search_summaries else None
    top_document_ok = (
        True
        if not case.expected_documents_contains
        else any(
            lower_text(needle) in lower_text(top_document_title)
            for needle in case.expected_documents_contains
        )
    )
    retrieval_contains_required = contains_all(aggregated_chunk_text, case.search_required_all)
    document_rank_metrics = compute_rank_metrics(
        ranked_document_keys,
        normalize_relevant_keys(case.relevant_documents),
    )
    chunk_rank_metrics = compute_marker_rank_metrics(ranked_chunk_texts, case.relevant_chunks)
    rank_metrics = {
        key: value
        for key, value in {
            "documents": document_rank_metrics,
            "chunks": chunk_rank_metrics,
        }.items()
        if value is not None
    }

    answer_has_required = contains_all(answer_text, case.answer_required_all) and (
        True if not case.answer_required_any else contains_any(answer_text, case.answer_required_any)
    )
    answer_opening = first_answer_sentence(answer_text)
    answer_opening_has_required = (
        True
        if not case.answer_opening_required_any
        else contains_any(answer_opening, case.answer_opening_required_any)
    )
    answer_has_forbidden = contains_any(answer_text, case.answer_forbidden_any)

    chunk_reference_count = len(execution_detail.get("chunkReferences", []))
    prepared_segment_reference_count = len(execution_detail.get("preparedSegmentReferences", []))
    technical_fact_reference_count = len(execution_detail.get("technicalFactReferences", []))
    entity_references = execution_detail.get("entityReferences", [])
    relation_references = execution_detail.get("relationReferences", [])
    entity_reference_count = len(entity_references)
    relation_reference_count = len(relation_references)
    entity_reference_labels_pass = references_contain_all_labels(
        entity_references,
        case.expected_entity_reference_labels_contains,
    )
    relation_reference_text_pass = relation_references_contain_all_text(
        relation_references,
        case.expected_relation_reference_text_contains,
    )
    verification_state = execution_detail.get("verificationState") or "not_run"
    verification_warnings = execution_detail.get("verificationWarnings", [])

    graph_usage_pass = (
        chunk_reference_count >= case.min_chunk_reference_count
        and entity_reference_count >= case.min_entity_reference_count
        and relation_reference_count >= case.min_relation_reference_count
    )
    structured_evidence_pass = (
        prepared_segment_reference_count >= case.min_prepared_segment_reference_count
        and technical_fact_reference_count >= case.min_technical_fact_reference_count
    )
    verification_pass = verification_state in case.allowed_verification_states
    failed_checks: list[str] = []
    append_failure_if(not top_document_ok, failed_checks, "top_document")
    append_failure_if(not retrieval_contains_required, failed_checks, "retrieval_contains_required")
    append_failure_if(not answer_has_required, failed_checks, "answer_required")
    append_failure_if(
        not answer_opening_has_required,
        failed_checks,
        "answer_opening_required",
    )
    append_failure_if(answer_has_forbidden, failed_checks, "answer_forbidden")
    append_failure_if(chunk_reference_count < case.min_chunk_reference_count, failed_checks, "chunk_references")
    append_failure_if(
        prepared_segment_reference_count < case.min_prepared_segment_reference_count,
        failed_checks,
        "prepared_segment_references",
    )
    append_failure_if(
        technical_fact_reference_count < case.min_technical_fact_reference_count,
        failed_checks,
        "technical_fact_references",
    )
    append_failure_if(
        entity_reference_count < case.min_entity_reference_count,
        failed_checks,
        "entity_references",
    )
    append_failure_if(
        relation_reference_count < case.min_relation_reference_count,
        failed_checks,
        "relation_references",
    )
    append_failure_if(
        not entity_reference_labels_pass,
        failed_checks,
        "entity_reference_labels",
    )
    append_failure_if(
        not relation_reference_text_pass,
        failed_checks,
        "relation_reference_text",
    )
    append_failure_if(not verification_pass, failed_checks, "verification_state")
    strict_case_pass = (
        top_document_ok
        and retrieval_contains_required
        and answer_has_required
        and answer_opening_has_required
        and not answer_has_forbidden
        and graph_usage_pass
        and entity_reference_labels_pass
        and relation_reference_text_pass
        and structured_evidence_pass
        and verification_pass
    )

    return {
        **case_metadata(case),
        "topSearchDocumentTitle": top_document_title,
        "topSearchDocumentOk": top_document_ok,
        "retrievalContainsRequired": retrieval_contains_required,
        "answerHasRequired": answer_has_required,
        "answerOpening": answer_opening,
        "answerOpeningHasRequired": answer_opening_has_required,
        "answerHasForbidden": answer_has_forbidden,
        "answerPass": answer_has_required and answer_opening_has_required and not answer_has_forbidden,
        "graphUsagePass": graph_usage_pass,
        "entityReferenceLabelsPass": entity_reference_labels_pass,
        "relationReferenceTextPass": relation_reference_text_pass,
        "structuredEvidencePass": structured_evidence_pass,
        "verificationState": verification_state,
        "verificationWarnings": verification_warnings,
        "verificationPass": verification_pass,
        "strictCasePass": strict_case_pass,
        "failedChecks": failed_checks,
        "searchResultCount": len(search_summaries),
        "searchLatencyMs": search_latency_ms,
        "searchResults": search_summaries,
        "rankMetrics": rank_metrics,
        "answerLatencyMs": answer_latency_ms,
        "answer": answer_text,
        "executionId": execution_id,
        "querySessionId": session_id,
        "executionState": execution.get("executionState") or execution.get("execution_state"),
        "chunkReferenceCount": chunk_reference_count,
        "preparedSegmentReferenceCount": prepared_segment_reference_count,
        "technicalFactReferenceCount": technical_fact_reference_count,
        "entityReferenceCount": entity_reference_count,
        "relationReferenceCount": relation_reference_count,
        "entityReferences": entity_references,
        "relationReferences": relation_references,
    }


def build_summary(case_results: list[dict[str, Any]]) -> dict[str, Any]:
    total = len(case_results)
    top_doc_pass = sum(1 for item in case_results if item["topSearchDocumentOk"])
    retrieval_pass = sum(1 for item in case_results if item["retrievalContainsRequired"])
    answer_pass = sum(1 for item in case_results if item["answerPass"])
    graph_usage_pass = sum(1 for item in case_results if item["graphUsagePass"])
    entity_label_pass = sum(1 for item in case_results if item["entityReferenceLabelsPass"])
    relation_text_pass = sum(1 for item in case_results if item["relationReferenceTextPass"])
    structured_evidence_pass = sum(1 for item in case_results if item["structuredEvidencePass"])
    verification_pass = sum(1 for item in case_results if item["verificationPass"])
    strict_case_pass = sum(1 for item in case_results if item["strictCasePass"])
    forbidden_failures = [item["caseId"] for item in case_results if item["answerHasForbidden"]]
    verification_failures = [item["caseId"] for item in case_results if not item["verificationPass"]]
    failure_reason_counts: dict[str, int] = {}
    for item in case_results:
        for failure_code in item.get("failedChecks", []):
            failure_reason_counts[failure_code] = failure_reason_counts.get(failure_code, 0) + 1
    rank_metrics = build_rank_metric_summary(case_results)
    answer_latency_ms = build_answer_latency_summary(case_results)
    return {
        "totalCases": total,
        "topDocumentPassCount": top_doc_pass,
        "retrievalPassCount": retrieval_pass,
        "answerPassCount": answer_pass,
        "graphUsagePassCount": graph_usage_pass,
        "entityReferenceLabelsPassCount": entity_label_pass,
        "relationReferenceTextPassCount": relation_text_pass,
        "structuredEvidencePassCount": structured_evidence_pass,
        "verificationPassCount": verification_pass,
        "strictCasePassCount": strict_case_pass,
        "topDocumentPassRate": round(top_doc_pass / total, 3) if total else 0.0,
        "retrievalPassRate": round(retrieval_pass / total, 3) if total else 0.0,
        "answerPassRate": round(answer_pass / total, 3) if total else 0.0,
        "graphUsagePassRate": round(graph_usage_pass / total, 3) if total else 0.0,
        "entityReferenceLabelsPassRate": round(entity_label_pass / total, 3) if total else 0.0,
        "relationReferenceTextPassRate": round(relation_text_pass / total, 3) if total else 0.0,
        "structuredEvidencePassRate": round(structured_evidence_pass / total, 3) if total else 0.0,
        "verificationPassRate": round(verification_pass / total, 3) if total else 0.0,
        "strictCasePassRate": round(strict_case_pass / total, 3) if total else 0.0,
        "forbiddenAnswerFailures": forbidden_failures,
        "verificationFailures": verification_failures,
        "failureReasonCounts": failure_reason_counts,
        "rankMetrics": rank_metrics,
        "answerLatencyMs": answer_latency_ms,
    }


def average(values: list[float]) -> float:
    return round(sum(values) / len(values), 6) if values else 0.0


def nearest_rank_percentile(values: list[float], percentile: int) -> float | None:
    """Return a conservative nearest-rank percentile for benchmark samples."""
    if not values:
        return None
    if percentile < 0 or percentile > 100:
        raise ValueError("percentile must be between 0 and 100")

    sorted_values = sorted(values)
    if percentile == 0:
        return round(sorted_values[0], 3)
    rank = math.ceil((percentile / 100.0) * len(sorted_values))
    return round(sorted_values[rank - 1], 3)


def build_answer_latency_summary(case_results: list[dict[str, Any]]) -> dict[str, Any]:
    """Aggregate valid per-case answerLatencyMs samples for JSON benchmark output."""
    latencies: list[float] = []
    for item in case_results:
        value = item.get("answerLatencyMs")
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            continue
        latency = float(value)
        if math.isfinite(latency) and latency >= 0.0:
            latencies.append(latency)

    return {
        "sampleCount": len(latencies),
        **{
            f"p{percentile}": nearest_rank_percentile(latencies, percentile)
            for percentile in ANSWER_LATENCY_PERCENTILES
        },
    }


def build_rank_metric_summary(case_results: list[dict[str, Any]]) -> dict[str, Any]:
    summary: dict[str, Any] = {}
    for family in ("documents", "chunks"):
        cases = [
            item.get("rankMetrics", {}).get(family)
            for item in case_results
            if item.get("rankMetrics", {}).get(family)
        ]
        family_metrics = [item for item in cases if isinstance(item, dict)]
        metric_summary: dict[str, Any] = {
            "caseCount": len(family_metrics),
            "mrr": average([float(item.get("mrr", 0.0)) for item in family_metrics]),
        }
        for cutoff in RANK_METRIC_CUTOFFS:
            key = f"hit@{cutoff}"
            metric_summary[key] = average(
                [1.0 if item.get(key) else 0.0 for item in family_metrics]
            )
        summary[family] = metric_summary
    return summary


def build_matrix_summary(suite_results: list[dict[str, Any]]) -> dict[str, Any]:
    total_suites = len(suite_results)
    strict_blocking_suites = sum(1 for suite in suite_results if suite["strictBlocking"])
    strict_blocking_suites_passed = sum(
        1
        for suite in suite_results
        if suite["strictBlocking"]
        and suite["summary"]["strictCasePassCount"] == suite["summary"]["totalCases"]
    )
    total_cases = sum(suite["summary"]["totalCases"] for suite in suite_results)
    strict_case_pass_count = sum(suite["summary"]["strictCasePassCount"] for suite in suite_results)
    failing_suites = [
        suite["suite"]["suiteId"]
        for suite in suite_results
        if suite["strictBlocking"]
        and suite["summary"]["strictCasePassCount"] != suite["summary"]["totalCases"]
    ]
    rank_metrics = build_matrix_rank_metric_summary(suite_results)
    answer_latency_ms = build_matrix_answer_latency_summary(suite_results)
    return {
        "totalSuites": total_suites,
        "strictBlockingSuites": strict_blocking_suites,
        "strictBlockingSuitesPassed": strict_blocking_suites_passed,
        "totalCases": total_cases,
        "strictCasePassCount": strict_case_pass_count,
        "strictCasePassRate": round(strict_case_pass_count / total_cases, 3)
        if total_cases
        else 0.0,
        "failingSuites": failing_suites,
        "rankMetrics": rank_metrics,
        "answerLatencyMs": answer_latency_ms,
    }


def build_matrix_rank_metric_summary(suite_results: list[dict[str, Any]]) -> dict[str, Any]:
    synthetic_cases: list[dict[str, Any]] = []
    for suite in suite_results:
        synthetic_cases.extend(suite.get("cases", []))
    return build_rank_metric_summary(synthetic_cases)


def build_matrix_answer_latency_summary(suite_results: list[dict[str, Any]]) -> dict[str, Any]:
    cases: list[dict[str, Any]] = []
    for suite in suite_results:
        cases.extend(suite.get("cases", []))
    return build_answer_latency_summary(cases)


def write_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def append_rank_trend(output_dir: Path, matrix_result: dict[str, Any]) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    record = {
        "generatedAt": matrix_result.get("generatedAt"),
        "suiteMatrix": matrix_result.get("suiteMatrix", []),
        "benchmarkIntegrity": matrix_result.get("benchmarkIntegrity"),
        "libraryId": (matrix_result.get("library") or {}).get("id"),
        "summary": matrix_result.get("summary", {}),
        "suites": [
            {
                "suiteId": suite.get("suite", {}).get("suiteId"),
                "integrity": suite.get("integrity"),
                "rankMetrics": suite.get("summary", {}).get("rankMetrics", {}),
            }
            for suite in matrix_result.get("suites", [])
        ],
    }
    with (output_dir / RANK_TREND_FILE_NAME).open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(record, ensure_ascii=False, sort_keys=True) + "\n")


def build_measurement_protocol(
    *,
    cache_policy: str,
    round_id: str,
    environment_identity: dict[str, Any],
    host_policy: dict[str, Any],
    host_preflight: dict[str, Any],
    host_midflight: dict[str, Any],
    host_postflight: dict[str, Any],
    busy_host_override: bool,
) -> dict[str, Any]:
    snapshots = {
        "pre": host_preflight,
        "mid": host_midflight,
        "post": host_postflight,
    }
    evaluations = {
        phase: evaluate_host_snapshot(snapshot, host_policy)
        for phase, snapshot in snapshots.items()
    }
    sequence_evaluation = evaluate_host_snapshot_sequence(snapshots)
    release_evidence_eligible = (
        not busy_host_override
        and not validate_environment_identity(environment_identity)
        and not validate_release_host_eligibility_policy(host_policy)
        and all(evaluation["passed"] for evaluation in evaluations.values())
        and sequence_evaluation["passed"]
    )
    return {
        "cachePolicy": cache_policy,
        "roundId": round_id,
        "sessionPolicy": "isolated_per_case",
        "caseOrderPolicy": CASE_ORDER_POLICY,
        "answerBeforeRankProbe": True,
        "environmentIdentity": environment_identity,
        "hostEligibilityPolicy": host_policy,
        "hostPreflight": host_preflight,
        "hostMidflight": host_midflight,
        "hostPostflight": host_postflight,
        "hostEligibility": evaluations,
        "hostSnapshotSequence": sequence_evaluation,
        "busyHostOverride": busy_host_override,
        "releaseEvidenceEligible": release_evidence_eligible,
    }


def main() -> int:
    args = parse_args()
    if not args.base_url:
        print(
            "IronRAG base URL is required via --base-url or IRONRAG_BENCHMARK_BASE_URL.",
            file=sys.stderr,
        )
        return 2
    if not args.workspace_id:
        print(
            "IronRAG workspace id is required via --workspace-id or IRONRAG_BENCHMARK_WORKSPACE_ID.",
            file=sys.stderr,
        )
        return 2
    try:
        session_cookie = resolve_session_cookie()
    except ValueError as error:
        print(str(error), file=sys.stderr)
        return 2
    if not valid_artifact_digest(args.runtime_artifact_digest):
        print(
            "IRONRAG_BENCHMARK_RUNTIME_ARTIFACT_DIGEST must be a non-empty "
            "immutable SHA-256 digest.",
            file=sys.stderr,
        )
        return 2
    runtime_artifact_digest = args.runtime_artifact_digest.strip()
    if not isinstance(args.round_id, str) or not args.round_id.strip():
        print("--round-id must be non-empty.", file=sys.stderr)
        return 2
    if args.query_top_k <= 0:
        print("--query-top-k must be positive.", file=sys.stderr)
        return 2
    if (
        not math.isfinite(args.wait_timeout_seconds)
        or args.wait_timeout_seconds <= 0.0
        or not math.isfinite(args.poll_interval_seconds)
        or args.poll_interval_seconds <= 0.0
    ):
        print("wait timeout and poll interval must be positive finite numbers.", file=sys.stderr)
        return 2
    if args.skip_upload and not args.library_id:
        print("--skip-upload requires --library-id.", file=sys.stderr)
        return 2
    if args.skip_upload and not args.canonicalize_reused_library:
        print(
            "--skip-upload requires --canonicalize-reused-library so latency is "
            "measured against a quiet corpus.",
            file=sys.stderr,
        )
        return 2
    if args.upload_only and args.skip_upload:
        print("--upload-only cannot be combined with --skip-upload.", file=sys.stderr)
        return 2
    host_policy = build_host_eligibility_policy(
        max_load_per_cpu=args.max_host_load_per_cpu,
        min_available_memory_percent=args.min_host_available_memory_percent,
        max_swap_used_percent=args.max_host_swap_used_percent,
        max_cpu_psi_some_avg10=args.max_host_cpu_psi_some_avg10,
        max_memory_psi_some_avg10=args.max_host_memory_psi_some_avg10,
        max_memory_psi_full_avg10=args.max_host_memory_psi_full_avg10,
        max_io_psi_some_avg10=args.max_host_io_psi_some_avg10,
        max_io_psi_full_avg10=args.max_host_io_psi_full_avg10,
    )
    invalid_host_policy = validate_host_eligibility_policy(host_policy)
    if invalid_host_policy:
        print(
            f"invalid host eligibility policy: {', '.join(invalid_host_policy)}",
            file=sys.stderr,
        )
        return 2
    weakened_host_policy = validate_release_host_eligibility_policy(host_policy)
    if weakened_host_policy and not args.allow_busy_host:
        print(
            "release host thresholds may only be made stricter; use "
            "--allow-busy-host for a diagnostic run: "
            + ", ".join(weakened_host_policy),
            file=sys.stderr,
        )
        return 2

    host_preflight = capture_host_snapshot()
    host_preflight_evaluation = evaluate_host_snapshot(host_preflight, host_policy)
    if not host_preflight_evaluation["passed"] and not args.allow_busy_host:
        print(
            json.dumps(
                {
                    "error": "benchmark_host_is_busy",
                    "hostEligibilityPolicy": host_policy,
                    "host": host_preflight,
                    "evaluation": host_preflight_evaluation,
                    "hint": (
                        "wait for a quiet host or use --allow-busy-host for a "
                        "non-release diagnostic run"
                    ),
                },
                ensure_ascii=False,
                indent=2,
            ),
            file=sys.stderr,
        )
        return 2

    suite_paths = [Path(item).resolve() for item in (args.suite or default_suite_paths())]
    suite_payloads = []
    all_documents: list[Path] = []
    missing_paths: list[str] = []
    for suite_path in suite_paths:
        documents, cases, payload = load_suite(suite_path)
        suite_payloads.append((suite_path, documents, cases, payload))
        for document_path in documents:
            if document_path not in all_documents:
                all_documents.append(document_path)
            if not document_path.exists():
                missing_paths.append(str(document_path))
    # Even a reused library needs the fixture bytes locally: without them the
    # result cannot carry a corpus fingerprint and must not be compared as a
    # no-regression measurement.
    if missing_paths:
        print(
            json.dumps(
                {"error": "missing_documents", "paths": sorted(set(missing_paths))},
                ensure_ascii=False,
                indent=2,
            ),
            file=sys.stderr,
        )
        return 2

    environment_identity = build_environment_identity(host_preflight)
    client = BenchmarkClient(args.base_url, session_cookie)
    runtime_version = client.get_json("/version")
    if not isinstance(runtime_version, dict) or any(
        not isinstance(runtime_version.get(key), str)
        or not runtime_version[key].strip()
        for key in ("service", "version", "environment", "role")
    ):
        print(
            "IronRAG /version response is missing service/version/environment/role identity.",
            file=sys.stderr,
        )
        return 2
    runtime_identity = {
        "label": args.runtime_label or runtime_version.get("version") or "unlabelled",
        "artifactDigest": runtime_artifact_digest,
        "versionEndpoint": runtime_version,
    }
    created_library = None
    library_id = args.library_id
    if not library_id:
        library_name = args.library_name or (
            f"Grounded Benchmark {datetime.now().strftime('%H%M%S')}"
        )
        created_library = create_library(client, args.workspace_id, library_name)
        library_id = created_library["id"]

    uploads = [] if args.skip_upload else upload_documents(client, library_id, all_documents)
    minimum_readable_count = len(all_documents)
    timeline, answer_ready_seconds, quiet_seconds = wait_for_library_state(
        client,
        library_id,
        minimum_readable_count,
        args.poll_interval_seconds,
        args.wait_timeout_seconds,
    )
    if answer_ready_seconds is None or quiet_seconds is None:
        print(
            json.dumps(
                {
                    "error": "benchmark_library_not_ready",
                    "expectedReadableDocumentCount": minimum_readable_count,
                    "lastState": timeline[-1] if timeline else None,
                },
                ensure_ascii=False,
                indent=2,
            ),
            file=sys.stderr,
        )
        return 2

    try:
        server_document_bytes = verify_server_corpus_bytes(
            client,
            library_id,
            all_documents,
        )
    except (RuntimeError, requests.RequestException) as error:
        print(
            json.dumps(
                {"error": "benchmark_corpus_verification_failed", "detail": str(error)},
                ensure_ascii=False,
                indent=2,
            ),
            file=sys.stderr,
        )
        return 2

    library_summary = fetch_library_summary(client, library_id)
    topology_counts = fetch_topology_counts(client, library_id)
    host_midflight = capture_host_snapshot()

    if args.upload_only:
        host_postflight = capture_host_snapshot()
        measurement_protocol = build_measurement_protocol(
            cache_policy=args.cache_policy,
            round_id=args.round_id,
            environment_identity=environment_identity,
            host_policy=host_policy,
            host_preflight=host_preflight,
            host_midflight=host_midflight,
            host_postflight=host_postflight,
            busy_host_override=args.allow_busy_host,
        )
        payload = {
            "generatedAt": utc_now_iso(),
            "mode": "upload_only",
            "workspaceId": args.workspace_id,
            "library": created_library or {"id": library_id},
            "uploadedDocumentCount": len(uploads),
            "uploadedDocumentPaths": [str(path) for path in all_documents],
            "pipeline": {
                "answerReadySeconds": answer_ready_seconds,
                "quietSeconds": quiet_seconds,
                "timeline": timeline,
            },
            "librarySummary": library_summary,
            "topologyCounts": topology_counts,
            "suitePaths": [str(path) for path in suite_paths],
            "runtimeIdentity": runtime_identity,
            "measurementProtocol": measurement_protocol,
        }
        if args.output:
            write_json(Path(args.output).resolve(), payload)
        if args.output_dir:
            write_json(Path(args.output_dir).resolve() / "upload.result.json", payload)
        print(json.dumps(payload, ensure_ascii=False, indent=2))
        return 0

    suite_results = []
    host_midflight = None
    completed_case_count = 0
    total_case_count = sum(len(cases) for _, _, cases, _ in suite_payloads)
    midpoint_case_count = max(1, math.ceil(total_case_count / 2))
    for suite_path, documents, cases, payload in suite_payloads:
        case_results = []
        query_session_ids = []
        ordered_cases = permute_cases_for_round(
            cases,
            args.round_id,
            str(payload.get("suiteId") or suite_path.stem),
        )
        for case in ordered_cases:
            session = create_query_session(client, args.workspace_id, library_id)
            query_session_ids.append(session["id"])
            case_results.append(
                run_case(client, library_id, session["id"], case, args.query_top_k)
            )
            completed_case_count += 1
            if host_midflight is None and completed_case_count >= midpoint_case_count:
                host_midflight = capture_host_snapshot()
        suite_result = {
            "generatedAt": utc_now_iso(),
            "suite": {
                "suiteId": payload.get("suiteId"),
                "description": payload.get("description"),
                "path": str(suite_path),
            },
            "strictBlocking": bool(payload.get("strictBlocking", True)),
            "integrity": build_suite_integrity(
                payload,
                cases,
                documents,
                server_document_bytes,
            ),
            "workspaceId": args.workspace_id,
            "library": created_library or {"id": library_id},
            "querySessionPolicy": "isolated_per_case",
            "caseOrderPolicy": CASE_ORDER_POLICY,
            "caseOrder": [case.case_id for case in ordered_cases],
            "querySessionIds": query_session_ids,
            "topologyCounts": topology_counts,
            "librarySummary": library_summary,
            "timing": {
                "answerReadySeconds": answer_ready_seconds,
                "pipelineQuietSeconds": quiet_seconds,
                "pollIntervalSeconds": args.poll_interval_seconds,
                "waitTimeoutSeconds": args.wait_timeout_seconds,
            },
            "summary": build_summary(case_results),
            "cases": case_results,
        }
        suite_results.append(suite_result)

    if host_midflight is None:
        host_midflight = capture_host_snapshot()
    host_postflight = capture_host_snapshot()
    measurement_protocol = build_measurement_protocol(
        cache_policy=args.cache_policy,
        round_id=args.round_id,
        environment_identity=environment_identity,
        host_policy=host_policy,
        host_preflight=host_preflight,
        host_midflight=host_midflight,
        host_postflight=host_postflight,
        busy_host_override=args.allow_busy_host,
    )
    release_evidence_eligible = measurement_protocol["releaseEvidenceEligible"]
    matrix_result = {
        "generatedAt": utc_now_iso(),
        "suiteMatrix": [str(path) for path in suite_paths],
        "workspaceId": args.workspace_id,
        "library": created_library or {"id": library_id},
        "benchmarkIntegrity": build_matrix_integrity(
            suite_results,
            query_top_k=args.query_top_k,
            skip_upload=args.skip_upload,
            canonicalize_reused_library=args.canonicalize_reused_library,
            cache_policy=args.cache_policy,
            round_id=args.round_id,
        ),
        "runtimeIdentity": runtime_identity,
        "measurementProtocol": measurement_protocol,
        "uploads": [
            {
                "documentId": item["document"]["document"]["id"],
                "fileName": item["document"]["fileName"],
                "jobId": item["mutation"].get("jobId"),
                "mutationId": item["mutation"]["mutation"]["id"],
            }
            for item in uploads
        ],
        "timing": {
            "answerReadySeconds": answer_ready_seconds,
            "pipelineQuietSeconds": quiet_seconds,
            "pollIntervalSeconds": args.poll_interval_seconds,
            "waitTimeoutSeconds": args.wait_timeout_seconds,
        },
        "opsTimeline": timeline,
        "topologyCounts": topology_counts,
        "librarySummary": library_summary,
        "summary": build_matrix_summary(suite_results),
        "suites": suite_results,
    }

    if args.output:
        write_json(Path(args.output), matrix_result)
    if args.output_dir:
        output_dir = Path(args.output_dir)
        write_json(output_dir / "matrix.result.json", matrix_result)
        for suite_result in suite_results:
            suite_path = Path(suite_result["suite"]["path"])
            write_json(output_dir / f"{suite_path.stem}.result.json", suite_result)
        append_rank_trend(output_dir, matrix_result)

    print(json.dumps(matrix_result, ensure_ascii=False, indent=2))

    if args.strict:
        if not release_evidence_eligible:
            return 1
        if (
            matrix_result["summary"]["strictBlockingSuites"]
            != matrix_result["summary"]["strictBlockingSuitesPassed"]
        ):
            return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
