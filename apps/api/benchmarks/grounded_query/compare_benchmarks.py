#!/usr/bin/env python3
"""Compare two golden benchmark result sets and print a diff report."""

import json
import sys
from pathlib import Path


def load_suite_results(result_dir: Path) -> dict:
    """Load all suite result files from a directory."""
    suites = {}
    for path in sorted(result_dir.glob("golden_*_suite.result.json")):
        data = json.loads(path.read_text())
        suite_id = data.get("suite", {}).get("suiteId", path.stem)
        suites[suite_id] = data
    return suites


def count_dimensions(cases: list) -> dict:
    """Count pass/fail per dimension across all cases."""
    dims = {}
    for case in cases:
        for dim_name, dim_val in case.get("dimensions", {}).items():
            if dim_name not in dims:
                dims[dim_name] = {"pass": 0, "fail": 0}
            if dim_val.get("pass"):
                dims[dim_name]["pass"] += 1
            else:
                dims[dim_name]["fail"] += 1
    return dims


def main():
    if len(sys.argv) < 3:
        print("Usage: compare_benchmarks.py <baseline_dir> <improved_dir>")
        sys.exit(1)

    baseline_dir = Path(sys.argv[1])
    improved_dir = Path(sys.argv[2])

    # Load matrix results
    baseline_matrix = json.loads((baseline_dir / "matrix.result.json").read_text())
    improved_matrix = json.loads((improved_dir / "matrix.result.json").read_text())

    print("=" * 80)
    print("BENCHMARK COMPARISON REPORT")
    print("=" * 80)

    # Graph topology comparison
    bt = baseline_matrix.get("topologyCounts", {})
    it = improved_matrix.get("topologyCounts", {})
    print("\n--- Graph Topology ---")
    for key in ["documents", "entities", "relations", "documentLinks"]:
        bv = bt.get(key, 0)
        iv = it.get(key, 0)
        delta = iv - bv
        pct = (delta / bv * 100) if bv else 0
        arrow = "↑" if delta > 0 else ("↓" if delta < 0 else "=")
        print(f"  {key:20s}  {bv:6d} → {iv:6d}  ({arrow} {abs(delta):+d}, {pct:+.0f}%)")

    # Per-suite case results
    baseline_suites = load_suite_results(baseline_dir)
    improved_suites = load_suite_results(improved_dir)

    total_baseline_pass = 0
    total_improved_pass = 0
    total_cases = 0

    for suite_id in sorted(set(list(baseline_suites.keys()) + list(improved_suites.keys()))):
        b_suite = baseline_suites.get(suite_id, {})
        i_suite = improved_suites.get(suite_id, {})
        b_cases = b_suite.get("cases", [])
        i_cases = i_suite.get("cases", [])

        print(f"\n--- {suite_id} ---")

        # Build case lookup
        b_case_map = {c["caseId"]: c for c in b_cases}
        i_case_map = {c["caseId"]: c for c in i_cases}

        all_case_ids = sorted(set(list(b_case_map.keys()) + list(i_case_map.keys())))

        for cid in all_case_ids:
            total_cases += 1
            bc = b_case_map.get(cid, {})
            ic = i_case_map.get(cid, {})

            b_dims = bc.get("dimensions", {})
            i_dims = ic.get("dimensions", {})

            b_pass = all(d.get("pass", False) for d in b_dims.values()) if b_dims else False
            i_pass = all(d.get("pass", False) for d in i_dims.values()) if i_dims else False

            if b_pass:
                total_baseline_pass += 1
            if i_pass:
                total_improved_pass += 1

            if b_pass and i_pass:
                status = "  PASS"
            elif not b_pass and i_pass:
                status = "↑ FIXED"
            elif b_pass and not i_pass:
                status = "↓ REGRESSED"
            else:
                status = "  FAIL"

            print(f"  [{status}] {cid}")

            if not i_pass:
                for dim_name, dim_val in i_dims.items():
                    if not dim_val.get("pass", False):
                        detail = dim_val.get("detail", dim_val.get("reason", ""))
                        print(f"          FAIL: {dim_name}: {str(detail)[:100]}")

    print(f"\n{'=' * 80}")
    print(f"OVERALL: Baseline {total_baseline_pass}/{total_cases} → Improved {total_improved_pass}/{total_cases}")
    delta = total_improved_pass - total_baseline_pass
    if delta > 0:
        print(f"  ↑ {delta} cases improved")
    elif delta < 0:
        print(f"  ↓ {abs(delta)} cases regressed")
    else:
        print(f"  = No change in pass rate")
    print(f"{'=' * 80}")


if __name__ == "__main__":
    main()
