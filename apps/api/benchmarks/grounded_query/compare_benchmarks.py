#!/usr/bin/env python3
"""Compare grounded benchmark results and enforce correctness and latency gates."""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
from typing import Any, Sequence

from run_live_benchmark import (
    BENCHMARK_INTEGRITY_SCHEMA_VERSION,
    CACHE_POLICIES,
    CASE_ORDER_POLICY,
    build_answer_latency_summary,
    build_matrix_integrity,
    evaluate_host_snapshot,
    evaluate_host_snapshot_sequence,
    normalized_artifact_digest,
    permute_case_ids_for_round,
    valid_artifact_digest,
    validate_environment_identity,
    validate_release_host_eligibility_policy,
)


RANK_METRIC_KEYS = ("mrr", "hit@1", "hit@3", "hit@5", "hit@10")
LATENCY_PERCENTILE_KEYS = ("p50", "p95", "p99")
DEFAULT_MAX_LATENCY_REGRESSION_PERCENT = 10.0
DEFAULT_MAX_CANDIDATE_P50_MS = 12_000.0
DEFAULT_MAX_CANDIDATE_P95_MS = 30_000.0


def valid_sha256(value: Any) -> bool:
    if not isinstance(value, str) or len(value) != 64:
        return False
    return all(character in "0123456789abcdef" for character in value)


def indexed_suites(matrix: dict[str, Any]) -> dict[str, dict[str, Any]]:
    indexed: dict[str, dict[str, Any]] = {}
    suites = matrix.get("suites")
    if not isinstance(suites, list):
        return indexed
    for suite in suites:
        if not isinstance(suite, dict):
            continue
        descriptor = suite.get("suite")
        suite_id = descriptor.get("suiteId") if isinstance(descriptor, dict) else None
        if not isinstance(suite_id, str) or not suite_id.strip():
            suite_id = "unknown-suite"
        if suite_id in indexed:
            raise ValueError(f"duplicate benchmark suite id: {suite_id}")
        indexed[suite_id] = suite
    return indexed


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Compare baseline and candidate grounded benchmark matrices. "
            "The command fails when correctness regresses or any answer-latency "
            "percentile exceeds the configured relative budget."
        )
    )
    parser.add_argument("baseline_dir", type=Path)
    parser.add_argument("candidate_dir", type=Path)
    parser.add_argument(
        "--max-latency-regression-percent",
        type=float,
        default=DEFAULT_MAX_LATENCY_REGRESSION_PERCENT,
        help="Maximum allowed relative regression for each of p50/p95/p99 (default: 10).",
    )
    parser.add_argument(
        "--json-output",
        type=Path,
        help="Optional path for the machine-readable comparison report.",
    )
    parser.add_argument(
        "--max-candidate-p50-ms",
        type=float,
        default=DEFAULT_MAX_CANDIDATE_P50_MS,
        help="Absolute candidate p50 ceiling in milliseconds (default: 12000).",
    )
    parser.add_argument(
        "--max-candidate-p95-ms",
        type=float,
        default=DEFAULT_MAX_CANDIDATE_P95_MS,
        help="Absolute candidate p95 ceiling in milliseconds (default: 30000).",
    )
    args = parser.parse_args(argv)
    if not math.isfinite(args.max_latency_regression_percent):
        parser.error("--max-latency-regression-percent must be finite")
    if args.max_latency_regression_percent < 0.0:
        parser.error("--max-latency-regression-percent must be non-negative")
    for option in ("max_candidate_p50_ms", "max_candidate_p95_ms"):
        value = getattr(args, option)
        if not math.isfinite(value) or value <= 0.0:
            parser.error(f"--{option.replace('_', '-')} must be a positive finite number")
    return args


def load_matrix(result_dir: Path) -> dict[str, Any]:
    path = result_dir / "matrix.result.json"
    payload = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise ValueError(f"benchmark matrix must be a JSON object: {path}")
    return payload


def case_passed(case: dict[str, Any]) -> bool:
    """Read the flat schema emitted by run_live_benchmark, with legacy fallback."""
    if "strictCasePass" in case:
        return case.get("strictCasePass") is True

    dimensions = case.get("dimensions")
    if not isinstance(dimensions, dict) or not dimensions:
        return False
    return all(
        isinstance(dimension, dict) and dimension.get("pass") is True
        for dimension in dimensions.values()
    )


def case_failure_details(case: dict[str, Any]) -> list[str]:
    """Read failure codes from the runner's flat schema, with legacy fallback."""
    failed_checks = case.get("failedChecks")
    if isinstance(failed_checks, list):
        return [str(item) for item in failed_checks]

    details: list[str] = []
    dimensions = case.get("dimensions")
    if not isinstance(dimensions, dict):
        return details
    for name, value in dimensions.items():
        if isinstance(value, dict) and value.get("pass") is not True:
            detail = value.get("detail", value.get("reason", ""))
            details.append(f"{name}: {detail}" if detail else str(name))
    return details


def indexed_cases(matrix: dict[str, Any]) -> dict[str, dict[str, Any]]:
    indexed: dict[str, dict[str, Any]] = {}
    for suite in matrix.get("suites", []):
        if not isinstance(suite, dict):
            continue
        suite_id = str((suite.get("suite") or {}).get("suiteId") or "unknown-suite")
        for case in suite.get("cases", []):
            if not isinstance(case, dict):
                continue
            case_id = str(case.get("caseId") or "unknown-case")
            # Case identifiers are only guaranteed to be unique inside a
            # suite. Always include the suite so comparison does not depend on
            # suite ordering when two suites reuse the same case id.
            key = f"{suite_id}/{case_id}"
            if key in indexed:
                raise ValueError(f"duplicate benchmark case key: {key}")
            indexed[key] = case
    return indexed


def validate_runtime_identity(
    label: str,
    matrix: dict[str, Any],
    invalid: list[str],
) -> None:
    runtime_identity = matrix.get("runtimeIdentity")
    if not isinstance(runtime_identity, dict):
        invalid.append(f"{label}.runtimeIdentity")
        return
    if not isinstance(runtime_identity.get("label"), str) or not runtime_identity[
        "label"
    ].strip():
        invalid.append(f"{label}.runtimeIdentity.label")
    if not valid_artifact_digest(runtime_identity.get("artifactDigest")):
        invalid.append(f"{label}.runtimeIdentity.artifactDigest")
    version_endpoint = runtime_identity.get("versionEndpoint")
    if not isinstance(version_endpoint, dict):
        invalid.append(f"{label}.runtimeIdentity.versionEndpoint")
        return
    for key in ("service", "version", "environment", "role"):
        value = version_endpoint.get(key)
        if not isinstance(value, str) or not value.strip():
            invalid.append(f"{label}.runtimeIdentity.versionEndpoint.{key}")


def validated_host_measurements(
    label: str,
    measurement_protocol: dict[str, Any],
    host_policy: object,
    invalid: list[str],
) -> tuple[dict[str, dict[str, Any]], dict[str, Any]]:
    host_eligibility: dict[str, dict[str, Any]] = {}
    host_snapshots: dict[str, dict[str, Any]] = {}
    for phase, field in (
        ("pre", "hostPreflight"),
        ("mid", "hostMidflight"),
        ("post", "hostPostflight"),
    ):
        snapshot = measurement_protocol.get(field)
        if not isinstance(snapshot, dict) or not isinstance(host_policy, dict):
            invalid.append(f"{label}.measurementProtocol.{field}")
            continue
        evaluation = evaluate_host_snapshot(snapshot, host_policy)
        host_snapshots[phase] = snapshot
        host_eligibility[phase] = evaluation
        if not evaluation["passed"]:
            invalid.append(f"{label}.measurementProtocol.{field}:ineligible")
    return host_eligibility, evaluate_host_snapshot_sequence(host_snapshots)


def validate_measurement_protocol(
    label: str,
    matrix: dict[str, Any],
    integrity: dict[str, Any],
    invalid: list[str],
) -> None:
    measurement_protocol = matrix.get("measurementProtocol")
    if not isinstance(measurement_protocol, dict):
        invalid.append(f"{label}.measurementProtocol")
        return
    for key in (
        "cachePolicy",
        "roundId",
        "sessionPolicy",
        "caseOrderPolicy",
        "answerBeforeRankProbe",
    ):
        if measurement_protocol.get(key) != integrity.get(key):
            invalid.append(f"{label}.measurementProtocol.{key}")

    environment_identity = measurement_protocol.get("environmentIdentity")
    environment_invalid = (
        validate_environment_identity(environment_identity)
        if isinstance(environment_identity, dict)
        else ["missing"]
    )
    invalid.extend(
        f"{label}.measurementProtocol.environmentIdentity.{item}"
        for item in environment_invalid
    )
    host_policy = measurement_protocol.get("hostEligibilityPolicy")
    host_policy_invalid = (
        validate_release_host_eligibility_policy(host_policy)
        if isinstance(host_policy, dict)
        else ["missing"]
    )
    invalid.extend(
        f"{label}.measurementProtocol.hostEligibilityPolicy.{item}"
        for item in host_policy_invalid
    )
    host_eligibility, snapshot_sequence = validated_host_measurements(
        label,
        measurement_protocol,
        host_policy,
        invalid,
    )
    if measurement_protocol.get("busyHostOverride") is not False:
        invalid.append(f"{label}.measurementProtocol.busyHostOverride")
    if not snapshot_sequence["passed"]:
        invalid.append(f"{label}.measurementProtocol.hostSnapshotSequence")

    independently_eligible = (
        not environment_invalid
        and not host_policy_invalid
        and len(host_eligibility) == 3
        and all(evaluation["passed"] for evaluation in host_eligibility.values())
        and measurement_protocol.get("busyHostOverride") is False
        and snapshot_sequence["passed"]
    )
    if measurement_protocol.get("hostEligibility") != host_eligibility:
        invalid.append(f"{label}.measurementProtocol.hostEligibility:stale")
    if measurement_protocol.get("hostSnapshotSequence") != snapshot_sequence:
        invalid.append(f"{label}.measurementProtocol.hostSnapshotSequence:stale")
    if measurement_protocol.get("releaseEvidenceEligible") is not independently_eligible:
        invalid.append(f"{label}.measurementProtocol.releaseEvidenceEligible:stale")
    if not independently_eligible:
        invalid.append(f"{label}.measurementProtocol.releaseEvidenceEligible")


def validate_matrix_integrity_fields(
    label: str,
    integrity: dict[str, Any],
    invalid: list[str],
) -> int | None:
    if integrity.get("schemaVersion") != BENCHMARK_INTEGRITY_SCHEMA_VERSION:
        invalid.append(f"{label}.benchmarkIntegrity.schemaVersion")
    query_top_k = integrity.get("queryTopK")
    if isinstance(query_top_k, bool) or not isinstance(query_top_k, int) or query_top_k <= 0:
        invalid.append(f"{label}.benchmarkIntegrity.queryTopK")
        query_top_k = None
    for key in ("skipUpload", "canonicalizeReusedLibrary"):
        if not isinstance(integrity.get(key), bool):
            invalid.append(f"{label}.benchmarkIntegrity.{key}")
    expected_values = {
        "sessionPolicy": "isolated_per_case",
        "caseOrderPolicy": CASE_ORDER_POLICY,
        "answerBeforeRankProbe": True,
    }
    for key, expected in expected_values.items():
        if integrity.get(key) != expected:
            invalid.append(f"{label}.benchmarkIntegrity.{key}")
    if integrity.get("cachePolicy") not in CACHE_POLICIES:
        invalid.append(f"{label}.benchmarkIntegrity.cachePolicy")
    round_id = integrity.get("roundId")
    if not isinstance(round_id, str) or not round_id.strip():
        invalid.append(f"{label}.benchmarkIntegrity.roundId")
    if not valid_sha256(integrity.get("matrixDefinitionSha256")):
        invalid.append(f"{label}.benchmarkIntegrity.matrixDefinitionSha256")
    return query_top_k


def validate_matrix_integrity(
    label: str,
    matrix: dict[str, Any],
    invalid: list[str],
) -> dict[str, Any] | None:
    initial_invalid_count = len(invalid)
    integrity = matrix.get("benchmarkIntegrity")
    if not isinstance(integrity, dict):
        invalid.append(f"{label}.benchmarkIntegrity")
        return None

    query_top_k = validate_matrix_integrity_fields(label, integrity, invalid)
    if len(invalid) > initial_invalid_count:
        return integrity

    recomputed = build_matrix_integrity(
        matrix.get("suites") if isinstance(matrix.get("suites"), list) else [],
        query_top_k=query_top_k,
        skip_upload=integrity["skipUpload"],
        canonicalize_reused_library=integrity["canonicalizeReusedLibrary"],
        cache_policy=integrity["cachePolicy"],
        round_id=integrity["roundId"],
    )
    if recomputed["matrixDefinitionSha256"] != integrity["matrixDefinitionSha256"]:
        invalid.append(f"{label}.benchmarkIntegrity.matrixDefinitionSha256:stale")
    validate_runtime_identity(label, matrix, invalid)
    validate_measurement_protocol(label, matrix, integrity, invalid)
    return integrity


def compare_matrix_integrities(
    baseline_integrity: dict[str, Any],
    candidate_integrity: dict[str, Any],
    mismatches: list[str],
) -> None:
    for key in (
        "schemaVersion",
        "queryTopK",
        "skipUpload",
        "canonicalizeReusedLibrary",
        "sessionPolicy",
        "caseOrderPolicy",
        "cachePolicy",
        "roundId",
        "answerBeforeRankProbe",
        "matrixDefinitionSha256",
    ):
        if baseline_integrity.get(key) != candidate_integrity.get(key):
            mismatches.append(f"matrix.{key}")


def compare_runtime_identities(
    baseline_matrix: dict[str, Any],
    candidate_matrix: dict[str, Any],
    mismatches: list[str],
) -> None:
    baseline_runtime = baseline_matrix.get("runtimeIdentity")
    candidate_runtime = candidate_matrix.get("runtimeIdentity")
    baseline_digest = (
        baseline_runtime.get("artifactDigest")
        if isinstance(baseline_runtime, dict)
        else None
    )
    candidate_digest = (
        candidate_runtime.get("artifactDigest")
        if isinstance(candidate_runtime, dict)
        else None
    )
    if (
        valid_artifact_digest(baseline_digest)
        and valid_artifact_digest(candidate_digest)
        and normalized_artifact_digest(baseline_digest)
        == normalized_artifact_digest(candidate_digest)
    ):
        mismatches.append("runtime.artifactDigest:notDistinct")
    baseline_version = (
        baseline_runtime.get("versionEndpoint")
        if isinstance(baseline_runtime, dict)
        else None
    )
    candidate_version = (
        candidate_runtime.get("versionEndpoint")
        if isinstance(candidate_runtime, dict)
        else None
    )
    if not isinstance(baseline_version, dict) or not isinstance(candidate_version, dict):
        return
    for key in ("service", "environment", "role"):
        if baseline_version.get(key) != candidate_version.get(key):
            mismatches.append(f"runtime.versionEndpoint.{key}")


def compare_measurement_protocols(
    baseline_matrix: dict[str, Any],
    candidate_matrix: dict[str, Any],
    mismatches: list[str],
) -> None:
    baseline_protocol = baseline_matrix.get("measurementProtocol")
    candidate_protocol = candidate_matrix.get("measurementProtocol")
    baseline_environment = (
        baseline_protocol.get("environmentIdentity")
        if isinstance(baseline_protocol, dict)
        else None
    )
    candidate_environment = (
        candidate_protocol.get("environmentIdentity")
        if isinstance(candidate_protocol, dict)
        else None
    )
    if (
        isinstance(baseline_environment, dict)
        and isinstance(candidate_environment, dict)
        and baseline_environment.get("fingerprintSha256")
        != candidate_environment.get("fingerprintSha256")
    ):
        mismatches.append("environmentIdentity.fingerprintSha256")
    if (
        isinstance(baseline_protocol, dict)
        and isinstance(candidate_protocol, dict)
        and baseline_protocol.get("hostEligibilityPolicy")
        != candidate_protocol.get("hostEligibilityPolicy")
    ):
        mismatches.append("measurementProtocol.hostEligibilityPolicy")


def validate_suite_integrity(
    label: str,
    suite_id: str,
    suite: dict[str, Any],
    invalid: list[str],
    round_id: Any,
) -> None:
    descriptor = suite.get("suite")
    raw_suite_id = descriptor.get("suiteId") if isinstance(descriptor, dict) else None
    if not isinstance(raw_suite_id, str) or not raw_suite_id.strip():
        invalid.append(f"{label}.suite:{suite_id}.suiteId")
    integrity = suite.get("integrity")
    if not isinstance(integrity, dict):
        invalid.append(f"{label}.suite:{suite_id}.integrity")
        return
    if integrity.get("schemaVersion") != BENCHMARK_INTEGRITY_SCHEMA_VERSION:
        invalid.append(f"{label}.suite:{suite_id}.schemaVersion")
    for key in ("suiteDefinitionSha256", "corpusSha256", "serverCorpusSha256"):
        if not valid_sha256(integrity.get(key)):
            invalid.append(f"{label}.suite:{suite_id}.{key}")
    if integrity.get("corpusVerification") != "server_source_bytes":
        invalid.append(f"{label}.suite:{suite_id}.corpusVerification")
    if integrity.get("corpusSha256") != integrity.get("serverCorpusSha256"):
        invalid.append(f"{label}.suite:{suite_id}.serverCorpusSha256:mismatch")
    validate_suite_case_order(label, suite_id, suite, round_id, invalid)


def validate_suite_integrities(
    label: str,
    suites: dict[str, dict[str, Any]],
    invalid: list[str],
    round_id: Any,
) -> None:
    for suite_id, suite in sorted(suites.items()):
        validate_suite_integrity(label, suite_id, suite, invalid, round_id)


def validate_suite_case_order(
    label: str,
    suite_id: str,
    suite: dict[str, Any],
    round_id: Any,
    invalid: list[str],
) -> None:
    case_order = suite.get("caseOrder")
    cases = suite.get("cases")
    expected_case_ids = (
        [case.get("caseId") for case in cases if isinstance(case, dict)]
        if isinstance(cases, list)
        else []
    )
    if suite.get("caseOrderPolicy") != CASE_ORDER_POLICY:
        invalid.append(f"{label}.suite:{suite_id}.caseOrderPolicy")
    valid_order = (
        isinstance(case_order, list)
        and all(isinstance(case_id, str) and case_id for case_id in case_order)
        and case_order == expected_case_ids
        and len(case_order) == len(set(case_order))
    )
    if not valid_order:
        invalid.append(f"{label}.suite:{suite_id}.caseOrder")
        return
    if case_order != permute_case_ids_for_round(case_order, str(round_id), suite_id):
        invalid.append(f"{label}.suite:{suite_id}.caseOrder:policyMismatch")


def compare_suite_integrities(
    baseline_suites: dict[str, dict[str, Any]],
    candidate_suites: dict[str, dict[str, Any]],
    mismatches: list[str],
) -> None:
    for suite_id in sorted(set(baseline_suites) & set(candidate_suites)):
        baseline_integrity = baseline_suites[suite_id].get("integrity") or {}
        candidate_integrity = candidate_suites[suite_id].get("integrity") or {}
        for key in (
            "schemaVersion",
            "suiteDefinitionSha256",
            "corpusSha256",
            "serverCorpusSha256",
            "corpusVerification",
        ):
            if baseline_integrity.get(key) != candidate_integrity.get(key):
                mismatches.append(f"suite:{suite_id}.{key}")


def validate_case_integrities(
    label: str,
    cases: dict[str, dict[str, Any]],
    invalid: list[str],
) -> None:
    for case_id, case in sorted(cases.items()):
        raw_case_id = case.get("caseId")
        if not isinstance(raw_case_id, str) or not raw_case_id.strip():
            invalid.append(f"{label}.case:{case_id}.caseId")
        if not valid_sha256(case.get("caseDefinitionSha256")):
            invalid.append(f"{label}.case:{case_id}.caseDefinitionSha256")


def compare_case_integrities(
    baseline_cases: dict[str, dict[str, Any]],
    candidate_cases: dict[str, dict[str, Any]],
    mismatches: list[str],
) -> None:
    for case_id in sorted(set(baseline_cases) & set(candidate_cases)):
        if baseline_cases[case_id].get("caseDefinitionSha256") != candidate_cases[
            case_id
        ].get("caseDefinitionSha256"):
            mismatches.append(f"case:{case_id}.caseDefinitionSha256")


def integrity_gate_passed(
    baseline_suites: dict[str, dict[str, Any]],
    candidate_suites: dict[str, dict[str, Any]],
    invalid_baseline: list[str],
    invalid_candidate: list[str],
    mismatches: list[str],
    missing_suites: list[str],
    added_suites: list[str],
    missing_cases: list[str],
    added_cases: list[str],
) -> bool:
    collections = (
        invalid_baseline,
        invalid_candidate,
        mismatches,
        missing_suites,
        added_suites,
        missing_cases,
        added_cases,
    )
    return bool(baseline_suites) and bool(candidate_suites) and not any(collections)


def build_benchmark_integrity_gate(
    baseline_matrix: dict[str, Any],
    candidate_matrix: dict[str, Any],
) -> dict[str, Any]:
    """Require identical benchmark definitions, runtime knobs, and corpus bytes."""
    invalid_baseline: list[str] = []
    invalid_candidate: list[str] = []
    mismatches: list[str] = []
    baseline_integrity = validate_matrix_integrity(
        "baseline", baseline_matrix, invalid_baseline
    )
    candidate_integrity = validate_matrix_integrity(
        "candidate", candidate_matrix, invalid_candidate
    )
    if baseline_integrity is not None and candidate_integrity is not None:
        compare_matrix_integrities(baseline_integrity, candidate_integrity, mismatches)
        compare_runtime_identities(baseline_matrix, candidate_matrix, mismatches)
        compare_measurement_protocols(baseline_matrix, candidate_matrix, mismatches)

    baseline_suites = indexed_suites(baseline_matrix)
    candidate_suites = indexed_suites(candidate_matrix)
    baseline_suite_ids = set(baseline_suites)
    candidate_suite_ids = set(candidate_suites)
    missing_suites = sorted(baseline_suite_ids - candidate_suite_ids)
    added_suites = sorted(candidate_suite_ids - baseline_suite_ids)
    validate_suite_integrities(
        "baseline",
        baseline_suites,
        invalid_baseline,
        baseline_integrity.get("roundId") if baseline_integrity else None,
    )
    validate_suite_integrities(
        "candidate",
        candidate_suites,
        invalid_candidate,
        candidate_integrity.get("roundId") if candidate_integrity else None,
    )
    compare_suite_integrities(baseline_suites, candidate_suites, mismatches)

    baseline_cases = indexed_cases(baseline_matrix)
    candidate_cases = indexed_cases(candidate_matrix)
    baseline_case_ids = set(baseline_cases)
    candidate_case_ids = set(candidate_cases)
    missing_cases = sorted(baseline_case_ids - candidate_case_ids)
    added_cases = sorted(candidate_case_ids - baseline_case_ids)
    validate_case_integrities("baseline", baseline_cases, invalid_baseline)
    validate_case_integrities("candidate", candidate_cases, invalid_candidate)
    compare_case_integrities(baseline_cases, candidate_cases, mismatches)

    invalid_baseline.sort()
    invalid_candidate.sort()
    mismatches.sort()
    return {
        "schemaVersion": BENCHMARK_INTEGRITY_SCHEMA_VERSION,
        "invalidBaseline": invalid_baseline,
        "invalidCandidate": invalid_candidate,
        "mismatches": mismatches,
        "missingSuites": missing_suites,
        "addedSuites": added_suites,
        "missingCases": missing_cases,
        "addedCases": added_cases,
        "passed": integrity_gate_passed(
            baseline_suites,
            candidate_suites,
            invalid_baseline,
            invalid_candidate,
            mismatches,
            missing_suites,
            added_suites,
            missing_cases,
            added_cases,
        ),
    }

def valid_case_latency(case: dict[str, Any]) -> float | None:
    value = case.get("answerLatencyMs")
    if (
        isinstance(value, bool)
        or not isinstance(value, (int, float))
        or not math.isfinite(float(value))
        or float(value) < 0.0
    ):
        return None
    return float(value)


def paired_latency_summaries(
    baseline_matrix: dict[str, Any],
    candidate_matrix: dict[str, Any],
) -> tuple[dict[str, Any], dict[str, Any], list[str]]:
    """Build percentiles only from the same baseline-labelled case identities."""
    baseline_cases = indexed_cases(baseline_matrix)
    candidate_cases = indexed_cases(candidate_matrix)
    baseline_samples: list[dict[str, Any]] = []
    candidate_samples: list[dict[str, Any]] = []
    missing_candidate_samples: list[str] = []

    for case_key, baseline_case in sorted(baseline_cases.items()):
        baseline_latency = valid_case_latency(baseline_case)
        if baseline_latency is None:
            continue
        baseline_samples.append({"answerLatencyMs": baseline_latency})
        candidate_case = candidate_cases.get(case_key)
        candidate_latency = (
            valid_case_latency(candidate_case) if candidate_case is not None else None
        )
        if candidate_latency is None:
            missing_candidate_samples.append(case_key)
            continue
        candidate_samples.append({"answerLatencyMs": candidate_latency})

    return (
        build_answer_latency_summary(baseline_samples),
        build_answer_latency_summary(candidate_samples),
        missing_candidate_samples,
    )


def latency_regression_percent(
    baseline_ms: float,
    candidate_ms: float,
) -> float | None:
    if baseline_ms < 0.0 or candidate_ms < 0.0:
        raise ValueError("latency values must be non-negative")
    if math.isclose(baseline_ms, 0.0, rel_tol=0.0, abs_tol=0.0):
        return 0.0 if math.isclose(candidate_ms, 0.0, rel_tol=0.0, abs_tol=0.0) else None
    return round(((candidate_ms - baseline_ms) / baseline_ms) * 100.0, 6)


def build_latency_gate(
    baseline: dict[str, Any],
    candidate: dict[str, Any],
    max_latency_regression_percent: float,
    *,
    missing_paired_samples: Sequence[str] = (),
    max_candidate_p50_ms: float = DEFAULT_MAX_CANDIDATE_P50_MS,
    max_candidate_p95_ms: float = DEFAULT_MAX_CANDIDATE_P95_MS,
) -> dict[str, Any]:
    percentile_results: dict[str, Any] = {}
    regressed_percentiles: list[str] = []
    absolute_budgets = {
        "p50": max_candidate_p50_ms,
        "p95": max_candidate_p95_ms,
    }
    breached_absolute_percentiles: list[str] = []

    for percentile in LATENCY_PERCENTILE_KEYS:
        baseline_value = baseline.get(percentile)
        candidate_value = candidate.get(percentile)
        valid = (
            not isinstance(baseline_value, bool)
            and isinstance(baseline_value, (int, float))
            and math.isfinite(float(baseline_value))
            and float(baseline_value) >= 0.0
            and not isinstance(candidate_value, bool)
            and isinstance(candidate_value, (int, float))
            and math.isfinite(float(candidate_value))
            and float(candidate_value) >= 0.0
        )
        regression_percent: float | None = None
        relative_passed = False
        if valid:
            regression_percent = latency_regression_percent(
                float(baseline_value),
                float(candidate_value),
            )
            relative_passed = (
                regression_percent is not None
                and regression_percent <= max_latency_regression_percent + 1e-9
            )
        absolute_budget = absolute_budgets.get(percentile)
        absolute_passed = (
            valid
            and (
                absolute_budget is None
                or float(candidate_value) <= absolute_budget + 1e-9
            )
        )
        percentile_passed = relative_passed and absolute_passed
        if not percentile_passed:
            regressed_percentiles.append(percentile)
        if absolute_budget is not None and not absolute_passed:
            breached_absolute_percentiles.append(percentile)

        percentile_results[percentile] = {
            "baselineMs": baseline_value,
            "candidateMs": candidate_value,
            "regressionPercent": regression_percent,
            "relativePassed": relative_passed,
            "absoluteBudgetMs": absolute_budget,
            "absolutePassed": absolute_passed,
            "passed": percentile_passed,
        }

    baseline_sample_count = baseline.get("sampleCount")
    candidate_sample_count = candidate.get("sampleCount")
    sample_count_preserved = (
        isinstance(baseline_sample_count, int)
        and not isinstance(baseline_sample_count, bool)
        and baseline_sample_count > 0
        and isinstance(candidate_sample_count, int)
        and not isinstance(candidate_sample_count, bool)
        and candidate_sample_count == baseline_sample_count
    )

    return {
        "maxRegressionPercent": max_latency_regression_percent,
        "baseline": baseline,
        "candidate": candidate,
        "sampleCountPreserved": sample_count_preserved,
        "missingPairedSamples": list(missing_paired_samples),
        "percentiles": percentile_results,
        "regressedPercentiles": regressed_percentiles,
        "breachedAbsolutePercentiles": breached_absolute_percentiles,
        "passed": sample_count_preserved and not regressed_percentiles,
    }


def valid_aggregate_rank_metric(value: Any) -> float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    normalized = float(value)
    if not math.isfinite(normalized) or not 0.0 <= normalized <= 1.0:
        return None
    return normalized


def evaluate_aggregate_rank_metrics(
    baseline_metrics: dict[str, Any],
    candidate_metrics: dict[str, Any],
) -> tuple[dict[str, Any], list[str], int]:
    metric_results: dict[str, Any] = {}
    regressed_metrics: list[str] = []
    evaluated_metric_count = 0
    for family in ("documents", "chunks"):
        baseline_family = baseline_metrics.get(family) or {}
        candidate_family = candidate_metrics.get(family) or {}
        baseline_case_count = baseline_family.get("caseCount", 0)
        if (
            isinstance(baseline_case_count, bool)
            or not isinstance(baseline_case_count, int)
            or baseline_case_count <= 0
        ):
            continue
        candidate_case_count = candidate_family.get("caseCount", 0)
        if (
            isinstance(candidate_case_count, bool)
            or not isinstance(candidate_case_count, int)
            or candidate_case_count != baseline_case_count
        ):
            regressed_metrics.append(f"{family}.caseCount")
        evaluated_metric_count += evaluate_aggregate_family(
            family,
            baseline_family,
            candidate_family,
            metric_results,
            regressed_metrics,
        )
    return metric_results, regressed_metrics, evaluated_metric_count


def evaluate_aggregate_family(
    family: str,
    baseline_family: dict[str, Any],
    candidate_family: dict[str, Any],
    metric_results: dict[str, Any],
    regressed_metrics: list[str],
) -> int:
    evaluated = 0
    for metric in RANK_METRIC_KEYS:
        if metric not in baseline_family:
            continue
        evaluated += 1
        baseline_value = baseline_family.get(metric)
        candidate_value = candidate_family.get(metric)
        normalized_baseline = valid_aggregate_rank_metric(baseline_value)
        normalized_candidate = valid_aggregate_rank_metric(candidate_value)
        passed = (
            normalized_baseline is not None
            and normalized_candidate is not None
            and normalized_candidate + 1e-12 >= normalized_baseline
        )
        key = f"{family}.{metric}"
        metric_results[key] = {
            "baseline": baseline_value,
            "candidate": candidate_value,
            "passed": passed,
        }
        if not passed:
            regressed_metrics.append(key)
    return evaluated


def evaluate_paired_rank_family(
    case_key: str,
    family: str,
    baseline_family: dict[str, Any],
    candidate_family: object,
    paired_metric_results: dict[str, Any],
    missing_paired_cases: list[str],
    regressed_paired_metrics: list[str],
) -> None:
    if not isinstance(candidate_family, dict) or not candidate_family:
        missing_paired_cases.append(f"{case_key}:{family}")
        return
    for metric in RANK_METRIC_KEYS:
        if metric not in baseline_family:
            continue
        baseline_value = normalized_case_rank_metric(baseline_family.get(metric))
        candidate_value = normalized_case_rank_metric(candidate_family.get(metric))
        passed = (
            baseline_value is not None
            and candidate_value is not None
            and candidate_value + 1e-12 >= baseline_value
        )
        metric_key = f"{case_key}:{family}.{metric}"
        paired_metric_results[metric_key] = {
            "baseline": baseline_value,
            "candidate": candidate_value,
            "passed": passed,
        }
        if not passed:
            regressed_paired_metrics.append(metric_key)


def evaluate_paired_rank_metrics(
    baseline_matrix: dict[str, Any],
    candidate_matrix: dict[str, Any],
) -> tuple[dict[str, Any], list[str], list[str]]:
    baseline_cases = indexed_cases(baseline_matrix)
    candidate_cases = indexed_cases(candidate_matrix)
    paired_metric_results: dict[str, Any] = {}
    missing_paired_cases: list[str] = []
    regressed_paired_metrics: list[str] = []
    for case_key, baseline_case in sorted(baseline_cases.items()):
        baseline_rank_metrics = baseline_case.get("rankMetrics")
        if not isinstance(baseline_rank_metrics, dict):
            continue
        candidate_case = candidate_cases.get(case_key)
        candidate_rank_metrics = (
            candidate_case.get("rankMetrics") if isinstance(candidate_case, dict) else None
        )
        for family in ("documents", "chunks"):
            baseline_family = baseline_rank_metrics.get(family)
            if not isinstance(baseline_family, dict) or not baseline_family:
                continue
            candidate_family = (
                candidate_rank_metrics.get(family)
                if isinstance(candidate_rank_metrics, dict)
                else None
            )
            evaluate_paired_rank_family(
                case_key,
                family,
                baseline_family,
                candidate_family,
                paired_metric_results,
                missing_paired_cases,
                regressed_paired_metrics,
            )
    return paired_metric_results, missing_paired_cases, regressed_paired_metrics


def build_retrieval_quality_gate(
    baseline_matrix: dict[str, Any],
    candidate_matrix: dict[str, Any],
) -> dict[str, Any]:
    """Fail when aggregate or same-case retrieval rank metrics decrease."""
    baseline_metrics = (baseline_matrix.get("summary") or {}).get("rankMetrics") or {}
    candidate_metrics = (candidate_matrix.get("summary") or {}).get("rankMetrics") or {}
    metric_results, regressed_metrics, evaluated_metric_count = (
        evaluate_aggregate_rank_metrics(baseline_metrics, candidate_metrics)
    )
    paired_metric_results, missing_paired_cases, regressed_paired_metrics = (
        evaluate_paired_rank_metrics(baseline_matrix, candidate_matrix)
    )
    regressed_metrics.sort()
    missing_paired_cases.sort()
    regressed_paired_metrics.sort()
    return {
        "evaluatedMetricCount": evaluated_metric_count,
        "metrics": metric_results,
        "regressedMetrics": regressed_metrics,
        "evaluatedPairedMetricCount": len(paired_metric_results),
        "pairedMetrics": paired_metric_results,
        "missingPairedCases": missing_paired_cases,
        "regressedPairedMetrics": regressed_paired_metrics,
        "passed": not regressed_metrics
        and not missing_paired_cases
        and not regressed_paired_metrics,
    }

def normalized_case_rank_metric(value: Any) -> float | None:
    if isinstance(value, bool):
        return 1.0 if value else 0.0
    if not isinstance(value, (int, float)) or not math.isfinite(float(value)):
        return None
    normalized = float(value)
    return normalized if 0.0 <= normalized <= 1.0 else None


def build_correctness_gate(
    baseline_matrix: dict[str, Any],
    candidate_matrix: dict[str, Any],
) -> dict[str, Any]:
    baseline_cases = indexed_cases(baseline_matrix)
    candidate_cases = indexed_cases(candidate_matrix)
    baseline_ids = set(baseline_cases)
    candidate_ids = set(candidate_cases)
    shared_ids = baseline_ids & candidate_ids

    regressed_cases = sorted(
        case_id
        for case_id in shared_ids
        if case_passed(baseline_cases[case_id]) and not case_passed(candidate_cases[case_id])
    )
    fixed_cases = sorted(
        case_id
        for case_id in shared_ids
        if not case_passed(baseline_cases[case_id]) and case_passed(candidate_cases[case_id])
    )
    missing_cases = sorted(baseline_ids - candidate_ids)
    added_cases = sorted(candidate_ids - baseline_ids)

    baseline_pass_count = sum(case_passed(case) for case in baseline_cases.values())
    candidate_pass_count = sum(case_passed(case) for case in candidate_cases.values())
    baseline_total = len(baseline_cases)
    candidate_total = len(candidate_cases)
    baseline_pass_rate = baseline_pass_count / baseline_total if baseline_total else 0.0
    candidate_pass_rate = candidate_pass_count / candidate_total if candidate_total else 0.0
    pass_rate_preserved = candidate_pass_rate + 1e-12 >= baseline_pass_rate
    candidate_all_strict_passed = (
        candidate_total > 0 and candidate_pass_count == candidate_total
    )

    return {
        "baseline": {
            "passedCases": baseline_pass_count,
            "totalCases": baseline_total,
            "passRate": round(baseline_pass_rate, 6),
        },
        "candidate": {
            "passedCases": candidate_pass_count,
            "totalCases": candidate_total,
            "passRate": round(candidate_pass_rate, 6),
        },
        "regressedCases": regressed_cases,
        "fixedCases": fixed_cases,
        "missingCases": missing_cases,
        "addedCases": added_cases,
        "passRatePreserved": pass_rate_preserved,
        "candidateAllStrictPassed": candidate_all_strict_passed,
        "passed": (
            baseline_total > 0
            and candidate_total > 0
            and not regressed_cases
            and not missing_cases
            and pass_rate_preserved
            and candidate_all_strict_passed
        ),
    }


def build_comparison_report(
    baseline_matrix: dict[str, Any],
    candidate_matrix: dict[str, Any],
    *,
    max_latency_regression_percent: float = DEFAULT_MAX_LATENCY_REGRESSION_PERCENT,
    max_candidate_p50_ms: float = DEFAULT_MAX_CANDIDATE_P50_MS,
    max_candidate_p95_ms: float = DEFAULT_MAX_CANDIDATE_P95_MS,
) -> dict[str, Any]:
    benchmark_integrity_gate = build_benchmark_integrity_gate(
        baseline_matrix,
        candidate_matrix,
    )
    baseline_latency, candidate_latency, missing_paired_samples = paired_latency_summaries(
        baseline_matrix,
        candidate_matrix,
    )
    latency_gate = build_latency_gate(
        baseline_latency,
        candidate_latency,
        max_latency_regression_percent,
        missing_paired_samples=missing_paired_samples,
        max_candidate_p50_ms=max_candidate_p50_ms,
        max_candidate_p95_ms=max_candidate_p95_ms,
    )
    correctness_gate = build_correctness_gate(baseline_matrix, candidate_matrix)
    retrieval_quality_gate = build_retrieval_quality_gate(baseline_matrix, candidate_matrix)
    return {
        "benchmarkIntegrityGate": benchmark_integrity_gate,
        "latencyGate": latency_gate,
        "correctnessGate": correctness_gate,
        "retrievalQualityGate": retrieval_quality_gate,
        "passed": (
            benchmark_integrity_gate["passed"]
            and latency_gate["passed"]
            and correctness_gate["passed"]
            and retrieval_quality_gate["passed"]
        ),
    }


def has_rank_metrics(metrics: dict[str, Any]) -> bool:
    return any(
        isinstance(metrics.get(family), dict)
        and int(metrics[family].get("caseCount", 0)) > 0
        for family in ("documents", "chunks")
    )


def print_rank_metric_delta(
    label: str,
    baseline_metrics: dict[str, Any],
    candidate_metrics: dict[str, Any],
) -> None:
    if not has_rank_metrics(baseline_metrics) and not has_rank_metrics(candidate_metrics):
        return

    print(label)
    for family in ("documents", "chunks"):
        baseline_family = baseline_metrics.get(family, {})
        candidate_family = candidate_metrics.get(family, {})
        if not baseline_family and not candidate_family:
            continue

        baseline_cases = int(baseline_family.get("caseCount", 0))
        candidate_cases = int(candidate_family.get("caseCount", 0))
        print(f"  {family} cases: {baseline_cases} -> {candidate_cases}")
        for key in RANK_METRIC_KEYS:
            baseline_value = float(baseline_family.get(key, 0.0))
            candidate_value = float(candidate_family.get(key, 0.0))
            delta = candidate_value - baseline_value
            print(
                f"    {key:6s} {baseline_value:.6f} -> "
                f"{candidate_value:.6f} ({delta:+.6f})"
            )


def print_graph_topology(
    baseline_matrix: dict[str, Any],
    candidate_matrix: dict[str, Any],
) -> None:
    baseline_topology = baseline_matrix.get("topologyCounts", {})
    candidate_topology = candidate_matrix.get("topologyCounts", {})
    print("\n--- Graph Topology ---")
    for key in ("documents", "entities", "relations", "documentLinks"):
        baseline_value = int(baseline_topology.get(key, 0))
        candidate_value = int(candidate_topology.get(key, 0))
        delta = candidate_value - baseline_value
        percent = (delta / baseline_value * 100.0) if baseline_value else 0.0
        arrow = "↑" if delta > 0 else ("↓" if delta < 0 else "=")
        print(
            f"  {key:20s}  {baseline_value:6d} → {candidate_value:6d}  "
            f"({arrow} {delta:+d}, {percent:+.0f}%)"
        )


def print_integrity_gate(integrity_gate: dict[str, Any]) -> None:
    print("\n--- Benchmark Integrity Gate ---")
    print("  " + ("PASS" if integrity_gate["passed"] else "FAIL"))
    labels = (
        ("invalidBaseline", "INVALID", ""),
        ("invalidCandidate", "INVALID", ""),
        ("mismatches", "MISMATCH", ""),
        ("missingSuites", "MISSING", "suite:"),
        ("addedSuites", "ADDED", "suite:"),
        ("missingCases", "MISSING", "case:"),
        ("addedCases", "ADDED", "case:"),
    )
    for field, label, prefix in labels:
        spacing = " " * (10 - len(label))
        for item in integrity_gate[field]:
            print(f"  [{label}]{spacing}{prefix}{item}")


def print_latency_gate(latency_gate: dict[str, Any]) -> None:
    print("\n--- Answer Latency Gate ---")
    for percentile in LATENCY_PERCENTILE_KEYS:
        result = latency_gate["percentiles"][percentile]
        regression = result["regressionPercent"]
        regression_text = "n/a" if regression is None else f"{regression:+.2f}%"
        status = "PASS" if result["passed"] else "REGRESSED"
        absolute_budget = result["absoluteBudgetMs"]
        absolute_text = "" if absolute_budget is None else f", ceiling {absolute_budget} ms"
        print(
            f"  [{status:9s}] {percentile}: {result['baselineMs']} ms -> "
            f"{result['candidateMs']} ms ({regression_text}{absolute_text})"
        )
    print(
        "  Budget: <= "
        f"{latency_gate['maxRegressionPercent']:.2f}% per percentile"
    )
    if not latency_gate["sampleCountPreserved"]:
        print("  [REGRESSED] paired latency sample count was not preserved")
    for case_id in latency_gate["missingPairedSamples"]:
        print(f"  [MISSING]   latency sample for {case_id}")


def print_retrieval_quality_gate(retrieval_gate: dict[str, Any]) -> None:
    print("\n--- Retrieval Quality Gate ---")
    if retrieval_gate["evaluatedMetricCount"] == 0:
        print("  No labelled rank metrics in baseline; gate not applicable")
    else:
        for metric, result in sorted(retrieval_gate["metrics"].items()):
            status = "PASS" if result["passed"] else "REGRESSED"
            print(
                f"  [{status:9s}] {metric}: "
                f"{result['baseline']} -> {result['candidate']}"
            )
    for case_id in retrieval_gate["missingPairedCases"]:
        print(f"  [MISSING]   paired rank metrics for {case_id}")
    for metric in retrieval_gate["regressedPairedMetrics"]:
        result = retrieval_gate["pairedMetrics"][metric]
        print(
            f"  [REGRESSED] {metric}: "
            f"{result['baseline']} -> {result['candidate']}"
        )


def print_correctness_gate(
    correctness_gate: dict[str, Any],
    candidate_matrix: dict[str, Any],
) -> None:
    baseline = correctness_gate["baseline"]
    candidate = correctness_gate["candidate"]
    print("\n--- Correctness Gate ---")
    print(
        "  Strict pass rate: "
        f"{baseline['passedCases']}/{baseline['totalCases']} "
        f"({baseline['passRate']:.3f}) -> "
        f"{candidate['passedCases']}/{candidate['totalCases']} "
        f"({candidate['passRate']:.3f})"
    )
    candidate_cases = indexed_cases(candidate_matrix)
    for case_id in correctness_gate["regressedCases"]:
        details = ", ".join(case_failure_details(candidate_cases.get(case_id, {})))
        print(f"  [REGRESSED] {case_id}: {details or 'unknown failure'}")
    for case_id in correctness_gate["fixedCases"]:
        print(f"  [FIXED]     {case_id}")
    for case_id in correctness_gate["missingCases"]:
        print(f"  [MISSING]   {case_id}")


def print_comparison_report(
    baseline_matrix: dict[str, Any],
    candidate_matrix: dict[str, Any],
    report: dict[str, Any],
) -> None:
    print("=" * 80)
    print("BENCHMARK COMPARISON REPORT")
    print("=" * 80)
    print_graph_topology(baseline_matrix, candidate_matrix)
    print_rank_metric_delta(
        "\n--- Retrieval Rank Metrics ---",
        baseline_matrix.get("summary", {}).get("rankMetrics", {}),
        candidate_matrix.get("summary", {}).get("rankMetrics", {}),
    )
    print_integrity_gate(report["benchmarkIntegrityGate"])
    print_latency_gate(report["latencyGate"])
    print_retrieval_quality_gate(report["retrievalQualityGate"])
    print_correctness_gate(report["correctnessGate"], candidate_matrix)
    print(f"\n{'=' * 80}")
    print("OVERALL: " + ("PASS" if report["passed"] else "FAIL"))
    print(f"{'=' * 80}")

def write_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2, allow_nan=False) + "\n",
        encoding="utf-8",
    )


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    baseline_matrix = load_matrix(args.baseline_dir)
    candidate_matrix = load_matrix(args.candidate_dir)
    report = build_comparison_report(
        baseline_matrix,
        candidate_matrix,
        max_latency_regression_percent=args.max_latency_regression_percent,
        max_candidate_p50_ms=args.max_candidate_p50_ms,
        max_candidate_p95_ms=args.max_candidate_p95_ms,
    )
    print_comparison_report(baseline_matrix, candidate_matrix, report)
    if args.json_output:
        write_json(args.json_output, report)
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
