"""Score a frozen normalized-span oracle against first native retrieval observations.

This reports core evidence availability, not generated answer correctness. A table
parent support ID alone does not expose its rows; only rendered direct supports
contribute source regions.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path

from generate_answers import checked_json
from semantic_boundaries import write_new


def covers(regions: list[tuple[int, int]], start: int, end: int) -> bool:
    cursor = start
    for left, right in sorted(regions):
        if right <= cursor:
            continue
        if left > cursor:
            return False
        cursor = max(cursor, right)
        if cursor >= end:
            return True
    return False


def direct_regions(treatment: dict, chunk: dict) -> list[tuple[int, int]]:
    blocks = {block["blockId"]: block for block in treatment["blocks"]}
    regions = []
    for identity in chunk["supportBlockIds"]:
        block = blocks[identity]
        if block["blockKind"] == "table" and chunk["chunkKind"] != "table":
            continue
        if block["normalizedText"].strip() not in chunk["normalizedText"]:
            continue
        span = block["sourceSpan"]
        if span is not None:
            regions.append((span["startOffset"], span["endOffset"]))
    return regions


def fact_available(fact: dict, regions: list[tuple[int, int]]) -> bool:
    return any(
        all(covers(regions, span["start"], span["end"]) for span in alternative)
        for alternative in fact["alternatives"]
    )


def validate_oracle(oracle: dict, export: dict) -> dict:
    if oracle["protocol"] != "irg256-normalized-evidence-oracle/1":
        raise ValueError("wrong normalized evidence oracle protocol")
    cases = {case["caseId"]: case for case in export["cases"]}
    queries = {}
    for query in oracle["queries"]:
        if query["queryId"] in queries or not query["facts"]:
            raise ValueError("duplicate query or empty fact checklist")
        case = cases[query["documentId"]]
        if query["normalizedSha256"] != case["normalizedSha256"]:
            raise ValueError("evidence oracle normalized identity mismatch")
        for fact in query["facts"]:
            if not fact["alternatives"] or any(not alt for alt in fact["alternatives"]):
                raise ValueError("empty fact evidence alternatives")
            for alternative in fact["alternatives"]:
                for span in alternative:
                    if (
                        not 0
                        <= span["start"]
                        < span["end"]
                        <= len(case["normalizedText"])
                    ):
                        raise ValueError("evidence span outside canonical source")
                    if (
                        case["normalizedText"][span["start"] : span["end"]]
                        != span["text"]
                    ):
                        raise ValueError("evidence oracle literal span mismatch")
        queries[query["queryId"]] = query
    return queries


def require_query_population(queries: dict, events: list[dict]) -> None:
    native_ids = {event["queryId"] for event in events if event.get("event") == "call"}
    if not queries or set(queries) != native_ids:
        raise ValueError("oracle must exactly cover the pinned native query population")
    headers = [event for event in events if event.get("event") == "run"]
    if len(headers) != 1 or events[-1]["queries"] != len(native_ids):
        raise ValueError("native declared query population is incomplete")


def score_call(call: dict, cases: dict, query: dict) -> dict:
    target = cases[query["documentId"]]
    accumulated = []
    grades = []
    cutoffs = {}
    for index, hit in enumerate(call["hits"]):
        regions = []
        if hit["caseId"] == target["caseId"]:
            matching = [
                row
                for row in target["treatments"]
                if row["treatmentId"] == hit["sourceTreatmentId"]
                and row["profileId"] == hit["profileId"]
            ]
            if len(matching) != 1:
                raise ValueError("native hit treatment identity mismatch")
            treatment = matching[0]
            chunk = treatment["chunks"][hit["chunkIndex"]]
            if chunk["literalDigest"] != hit["literalDigest"]:
                raise ValueError("native hit literal digest mismatch")
            regions = direct_regions(treatment, chunk)
        grades.append(sum(fact_available(fact, regions) for fact in query["facts"]))
        accumulated.extend(regions)
        if index + 1 in (5, 10):
            cutoffs[str(index + 1)] = [
                fact_available(fact, accumulated) for fact in query["facts"]
            ]
    for cutoff in (5, 10):
        cutoffs.setdefault(
            str(cutoff), [fact_available(fact, accumulated) for fact in query["facts"]]
        )
    return {
        "queryId": query["queryId"],
        "documentId": query["documentId"],
        "factCount": len(query["facts"]),
        "coreFactCoverage": cutoffs,
        "reciprocalFirstEvidenceRank": next(
            (1 / (i + 1) for i, grade in enumerate(grades) if grade), 0
        ),
        "documentHitAt5": any(
            hit["caseId"] == target["caseId"] for hit in call["hits"][:5]
        ),
        "documentHitAt10": any(
            hit["caseId"] == target["caseId"] for hit in call["hits"]
        ),
        "coreFactGradesByRank": grades,
        "discountedCoreFactGainAt10": sum(
            grade / math.log2(i + 2) for i, grade in enumerate(grades)
        ),
        "normalizedContextCharsAt5": sum(
            hit["normalizedChars"] for hit in call["hits"][:5]
        ),
        "nativeLatencyMs": call["latencyMs"],
    }


def run(args: argparse.Namespace) -> None:
    export = checked_json(args.treatments, args.treatments_sha256, 64 * 1024 * 1024)
    oracle = checked_json(args.oracle, args.oracle_sha256)
    queries = validate_oracle(oracle, export)
    if oracle["canonicalInputSha256"] != export["canonicalInputSha256"]:
        raise ValueError("oracle and treatment preparation differ")
    if args.retrieval.stat().st_size > 64 * 1024 * 1024:
        raise ValueError("native replay journal exceeds bounded input")
    if hashlib.sha256(args.retrieval.read_bytes()).hexdigest() != args.retrieval_sha256:
        raise ValueError("native retrieval journal pin mismatch")
    events = [
        json.loads(line) for line in args.retrieval.read_text().splitlines() if line
    ]
    if not events or events[-1].get("event") != "complete":
        raise ValueError("native replay has no terminal success; preserve its failure")
    observed_calls = [event for event in events if event.get("event") == "call"]
    if len(observed_calls) != events[-1]["calls"]:
        raise ValueError("native replay journal lacks its declared call population")
    require_query_population(queries, events)
    header = events[0]
    if header["treatmentExportSha256"] != args.treatments_sha256:
        raise ValueError("native replay used a different treatment export")
    cases = {case["caseId"]: case for case in export["cases"]}
    rows = []
    for query_id, query in queries.items():
        calls = [
            event
            for event in events
            if event.get("event") == "call"
            and event["queryId"] == query_id
            and event["phase"] == "measured"
        ]
        if not calls:
            raise ValueError("native replay lacks a frozen query")
        if any(call["query"] != query["question"] for call in calls):
            raise ValueError("native query was rewritten")
        groups = {}
        for call in calls:
            groups.setdefault((call["treatmentId"], call["profileId"]), []).append(
                call["callIndex"]
            )
        if any(sorted(indices) != [1, 2, 3] for indices in groups.values()):
            raise ValueError(
                "native arm lacks exactly three first measured observations"
            )
        for call in calls:
            if not call["targetInTreatmentScope"]:
                continue
            row = score_call(call, cases, query)
            row.update(
                {
                    "treatmentId": call["treatmentId"],
                    "profileId": call["profileId"],
                    "callIndex": call["callIndex"],
                }
            )
            rows.append(row)
    write_new(
        args.output,
        {
            "protocol": "irg256-core-evidence-scores/1",
            "sourceSha": export["sourceSha"],
            "treatmentExportSha256": args.treatments_sha256,
            "oracleSha256": args.oracle_sha256,
            "nativeReplaySha256": args.retrieval_sha256,
            "scores": rows,
            "scope": "Direct rendered-support core source-span coverage, not window expansion or generated-answer correctness",
            "providerCalls": 0,
        },
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("treatments", "oracle", "retrieval"):
        parser.add_argument(f"--{name}", required=True, type=Path)
        parser.add_argument(f"--{name}-sha256", required=True)
    parser.add_argument("--output", required=True, type=Path)
    run(parser.parse_args())


if __name__ == "__main__":
    main()
