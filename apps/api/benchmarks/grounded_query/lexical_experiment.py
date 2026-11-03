#!/usr/bin/env python3
"""Prepare frozen lexical replay inputs and evaluate native replay receipts offline."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import statistics
from pathlib import Path
from typing import Any
from uuid import NAMESPACE_URL, uuid5

try:
    from .run_live_benchmark import compute_rank_metrics, load_suite
except ImportError:  # Direct script execution has no package context.
    from run_live_benchmark import compute_rank_metrics, load_suite


ROOT = Path(__file__).resolve().parent
ARMS = ("production_fts", "raw_fts", "raw_bm25")


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def manifest_digest(manifest: dict[str, Any]) -> str:
    payload = {key: value for key, value in manifest.items() if key != "manifest_sha256"}
    return sha256(json.dumps(payload, sort_keys=True, ensure_ascii=False,
                            separators=(",", ":"), allow_nan=False).encode("utf-8"))


def quote_token(token: str) -> str:
    return '"' + token.replace("\\", "\\\\").replace('"', '\\"') + '"'


def ordinary_bm25(query: str) -> str:
    tokens = query.split()
    if not tokens or any(token.upper() in {"AND", "OR", "NOT"} or
                         token.startswith("-") or any(c in token for c in '\"()')
                         for token in tokens):
        raise ValueError("query has phrase/Boolean syntax requiring explicit translation")
    return " AND ".join(map(quote_token, tokens))


def prepare() -> dict[str, Any]:
    suite = ROOT / "golden_hardening_suite.json"
    paths, cases, _ = load_suite(suite)
    documents = []
    excluded = []
    for path in paths:
        relative = path.relative_to(ROOT).as_posix()
        if path.suffix.lower() in {".pdf", ".png"}:
            excluded.append({"path": relative, "reason": "binary fixture; extraction not measured"})
            continue
        data = path.read_bytes()
        documents.append({
            "key": path.name, "path": relative, "sha256": sha256(data),
            "text": data.decode("utf-8"), "title": path.stem,
            **{f"{kind}_id": str(uuid5(NAMESPACE_URL, f"irg285:{relative}:{kind}"))
               for kind in ("document", "revision", "chunk")},
        })
    selected = {doc["key"] for doc in documents}
    queries = []
    excluded_cases = []
    for case in cases:
        try:
            if not case.relevant_documents:
                raise ValueError("no document relevance")
            resolved = []
            for marker in case.expected_documents_contains + case.relevant_documents:
                matches = [path.name for path in paths if marker in path.name]
                if not matches or not set(matches) <= selected:
                    raise ValueError("expected/relevant document outside UTF-8 corpus or unresolved")
            for marker in case.relevant_documents:
                resolved.extend(path.name for path in paths if marker in path.name)
            bm25 = ordinary_bm25(case.search_query)
        except ValueError as error:
            excluded_cases.append({"id": case.case_id, "reason": str(error)})
            continue
        queries.append({"id": case.case_id, "query": case.search_query, "bm25_query": bm25,
                        "relevant_documents": sorted(set(resolved)), "excluded_documents": []})

    # These probes use existing fixture literals, not a product routing vocabulary.
    controls = [
        ("control_boolean_positive", "relay cumulative", '"relay" AND "cumulative"',
         ["relay_twin_standard.md"], ["relay_twin_extended.md"]),
        ("control_boolean_not_cumulative", "relay -cumulative", '"relay" AND -"cumulative"',
         ["relay_twin_extended.md"], ["relay_twin_standard.md"]),
        ("control_boolean_not_immediate", "relay -immediate", '"relay" AND -"immediate"',
         ["relay_twin_standard.md"], ["relay_twin_extended.md"]),
        ("control_rare_identifier", "coalesce_shard_windows", '"coalesce_shard_windows"',
         ["lattice_pipeline_utils.py"], []),
        ("control_compound_identifier", "RLY-STD-204", '"RLY-STD-204"',
         ["relay_twin_standard.md"], ["relay_twin_extended.md"]),
        ("control_near_twin_phrase", '"frame window" immediate', '"frame window" AND "immediate"',
         ["relay_twin_extended.md"], ["relay_twin_standard.md"]),
    ]
    for case_id, query, bm25, relevant, forbidden in controls:
        queries.append({"id": case_id, "query": query, "bm25_query": bm25,
                        "relevant_documents": relevant, "excluded_documents": forbidden})
    manifest = {
        "schema_version": 1, "documents": documents, "queries": queries,
        "limit": 10, "warmups": 2, "repetitions": 15,
        "source_suite_sha256": sha256(suite.read_bytes()),
        "rank_relevance_sha256": sha256((ROOT / "rank_relevance.json").read_bytes()),
        "excluded_fixtures": excluded,
        "settings": {
            "analyzer": "simple", "chunk_policy": "one UTF-8 file per ready chunk",
            "content_text": "document.text", "normalized_text": "document.text",
            "window_text": "", "section_path": "", "heading_text": "",
            "raw_treatment_text": "content_text + ' ' + normalized_text + '   '",
            "excluded_cases": excluded_cases,
            "control_intent": {row[0]: {"original_pg_query": row[1],
                                       "include": row[3], "exclude": row[4]}
                               for row in controls},
            "boolean_mapping": "PostgreSQL websearch minus exclusion maps to native AND -\"term\"; "
                               "pg_search 0.26.2 uses minus exclusion, not a NOT keyword.",
            "comparison_limits": [
                "PostgreSQL websearch and native BM25 have different tokenizers/analyzers; "
                "quoted native tokens, especially compound identifiers, may be phrases.",
                "Explicit phrase probes preserve phrase intent, not identical phrase semantics.",
                "Full product FTS versus raw BM25 is not a policy-matched replacement comparison "
                "and does not isolate scorer-only effects.",
            ],
        },
    }
    manifest["manifest_sha256"] = manifest_digest(manifest)
    validate_manifest(manifest)
    return manifest


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def number(value: Any, label: str, *, nonnegative: bool = True) -> None:
    require(type(value) in (int, float) and math.isfinite(value) and
            (not nonnegative or value >= 0), f"invalid {label}")


def unique_strings(values: Any, label: str, *, nonempty: bool = False) -> None:
    require(isinstance(values, list) and (bool(values) or not nonempty), f"invalid {label}")
    require(all(isinstance(value, str) and value for value in values), f"invalid {label}")
    require(len(values) == len(set(values)), f"duplicate {label}")


def validate_manifest(manifest: Any) -> None:
    require(isinstance(manifest, dict), "manifest must be an object")
    require(type(manifest.get("schema_version")) is int and manifest["schema_version"] == 1,
            "unsupported manifest schema")
    require(manifest.get("manifest_sha256") == manifest_digest(manifest), "manifest checksum mismatch")
    for key, expected in (("limit", 10), ("warmups", 2), ("repetitions", 15)):
        require(type(manifest.get(key)) is int and manifest[key] == expected, f"invalid {key}")
    documents = manifest.get("documents")
    require(isinstance(documents, list) and bool(documents), "missing documents")
    for field in ("key", "path", "document_id", "revision_id", "chunk_id"):
        unique_strings([doc.get(field) for doc in documents if isinstance(doc, dict)], field, nonempty=True)
        require(all(isinstance(doc, dict) for doc in documents), "invalid document")
    keys = {doc["key"] for doc in documents}
    for doc in documents:
        path = Path(doc["path"])
        require(not path.is_absolute() and ".." not in path.parts and
                path.name == doc["key"], "invalid document path/key")
        require(path.suffix.lower() not in {".pdf", ".png"}, "binary fixture in manifest")
        require(isinstance(doc.get("text"), str), "invalid document text")
        require(sha256(doc["text"].encode("utf-8")) == doc.get("sha256"), "document checksum mismatch")
        require(doc.get("title") == path.stem, "document title mismatch")
        for kind in ("document", "revision", "chunk"):
            require(doc[f"{kind}_id"] == str(uuid5(NAMESPACE_URL, f"irg285:{doc['path']}:{kind}")),
                    f"invalid {kind} identity")
    queries = manifest.get("queries")
    require(isinstance(queries, list) and bool(queries), "missing queries")
    require(all(isinstance(query, dict) for query in queries), "invalid query")
    unique_strings([query.get("id") for query in queries], "query IDs", nonempty=True)
    for query in queries:
        for field in ("query", "bm25_query"):
            require(isinstance(query.get(field), str) and bool(query[field].strip()), f"invalid {field}")
        for field in ("relevant_documents", "excluded_documents"):
            unique_strings(query.get(field), field, nonempty=field == "relevant_documents")
            require(set(query[field]) <= keys, f"unknown {field}")
        require(not set(query["relevant_documents"]) & set(query["excluded_documents"]),
                "overlapping relevance and exclusion")


def percentile95(samples: list[float]) -> float:
    """Nearest-rank p95; no interpolation or cross-query weighting."""
    return sorted(samples)[math.ceil(0.95 * len(samples)) - 1]


def evaluate(manifest: dict[str, Any], receipt: dict[str, Any]) -> dict[str, Any]:
    validate_manifest(manifest)
    require(isinstance(receipt, dict), "receipt must be an object")
    require(type(receipt.get("schema_version")) is int and receipt["schema_version"] == 1,
            "unsupported receipt schema")
    require(receipt.get("manifest_sha256") == manifest["manifest_sha256"], "receipt manifest mismatch")
    unique_strings(receipt.get("arms"), "arms", nonempty=True)
    require(set(receipt["arms"]) == set(ARMS), "unknown/missing arms")
    versions = receipt.get("versions")
    require(isinstance(versions, dict) and all(isinstance(versions.get(key), str) and versions[key]
            for key in ("postgres", "pg_search", "vector")), "missing versions")
    require(isinstance(receipt.get("settings"), dict), "missing settings")
    for field in ("setup_ms", "index_bytes"):
        values = receipt.get(field)
        require(isinstance(values, dict) and bool(values), f"missing {field}")
        for value in values.values():
            number(value, field)
            if field == "index_bytes":
                require(type(value) is int, "index size must be integer bytes")
    chunks = {doc["chunk_id"]: doc for doc in manifest["documents"]}
    queries = {query["id"]: query for query in manifest["queries"]}
    expected = {(arm, query_id) for arm in ARMS for query_id in queries}
    seen = set()
    results = receipt.get("results")
    require(isinstance(results, list), "missing results")
    per_arm: dict[str, list[dict[str, Any]]] = {arm: [] for arm in ARMS}
    pooled: dict[str, list[float]] = {arm: [] for arm in ARMS}
    for result_index, result in enumerate(results):
        require(isinstance(result, dict), "invalid result")
        arm, query_id = result.get("arm"), result.get("query_id")
        require(isinstance(arm, str) and isinstance(query_id, str), "invalid result identity")
        pair = (arm, query_id)
        require(pair in expected and pair not in seen, "unknown/duplicate arm-query pair")
        seen.add(pair)
        samples = result.get("samples_ms")
        require(isinstance(samples, list) and len(samples) == manifest["repetitions"], "invalid sample count")
        for sample in samples:
            number(sample, "latency sample")
        hits = result.get("hits")
        require(isinstance(hits, list) and len(hits) <= manifest["limit"], "invalid hits count")
        seen_chunks = set()
        duplicates = []
        ranked = []
        for hit in hits:
            require(isinstance(hit, dict) and isinstance(hit.get("chunk_id"), str), "invalid hit")
            chunk = hit["chunk_id"]
            require(chunk in chunks, "unknown chunk")
            if chunk in seen_chunks:
                require(arm == "production_fts", "duplicate chunk in diagnostic pair")
                if chunk not in duplicates:
                    duplicates.append(chunk)
            seen_chunks.add(chunk)
            doc = chunks[chunk]
            require(hit.get("document_key") == doc["key"], "hit document mismatch")
            require(hit.get("text_sha256") == doc["sha256"], "hit checksum mismatch")
            number(hit.get("score"), "score", nonnegative=False)
            if doc["key"] not in ranked:
                ranked.append(doc["key"])
        query = queries[query_id]
        # Product reference anomalies must not contaminate or invalidate the
        # independently complete raw pair, nor become an accepted product score.
        metrics = None if duplicates else compute_rank_metrics(ranked, set(query["relevant_documents"]))
        per_arm[arm].append({"query_id": query_id, "rank_metrics": metrics,
                             "status": "invalid_reference" if duplicates else "measured",
                             "duplicate_chunk_ids": duplicates, "native_result_index": result_index,
                             "excluded_hits": [key for key in ranked if key in query["excluded_documents"]],
                             "latency_ms": {"median": statistics.median(samples), "p95": percentile95(samples)}})
        pooled[arm].extend(samples)
    require(seen == expected, "missing arm-query pairs")
    summaries = {}
    for arm, rows in per_arm.items():
        metrics = [row["rank_metrics"] for row in rows]
        invalid = [row["query_id"] for row in rows if row["status"] == "invalid_reference"]
        summaries[arm] = {
            "status": "invalid_reference" if invalid else "measured",
            "invalid_query_ids": sorted(invalid),
            "queries": sorted(rows, key=lambda row: row["query_id"]),
            "aggregate_rank_metrics": None if invalid else {
                key: statistics.mean(metric[key] for metric in metrics)
                for key in metrics[0] if key == "mrr" or key.startswith("hit@")},
            "exclusion_violation_count": sum(len(row["excluded_hits"]) for row in rows),
            "latency_context": "observed invalid-reference path" if invalid else "measured treatment path",
            "warm_client_latency_ms": {"median": statistics.median(pooled[arm]),
                                       "p95": percentile95(pooled[arm])},
        }
    return {"schema_version": 1, "manifest_sha256": manifest["manifest_sha256"],
            "status": "partial_diagnostic" if summaries["production_fts"]["invalid_query_ids"] else "measured",
            "diagnostic_pair_status": "measured",
            "production_reference_status": summaries["production_fts"]["status"],
            "arms": summaries, "versions": versions, "settings": receipt["settings"],
            "setup_ms": receipt["setup_ms"], "index_bytes": receipt["index_bytes"],
            "latency_method": "warm client samples; pooled equal repetitions; p95 nearest rank; setup separate",
            "rank_method": "document first occurrence; equal-query mean of canonical compute_rank_metrics",
            "final_answer": "not measured", "UI": "not measured", "MCP": "not measured",
            "adoption": "not decided", "comparison_limits": manifest.get("settings", {}).get("comparison_limits", [])}


def read_json(path: Path) -> Any:
    def invalid_constant(value: str) -> Any:
        raise ValueError(f"nonfinite JSON constant: {value}")

    def object_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result = {}
        for key, value in pairs:
            require(key not in result, f"duplicate JSON key: {key}")
            result[key] = value
        return result
    return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=object_pairs,
                      parse_constant=invalid_constant)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)
    prepare_parser = subcommands.add_parser("prepare")
    prepare_parser.add_argument("--output", required=True, type=Path)
    evaluate_parser = subcommands.add_parser("evaluate")
    evaluate_parser.add_argument("--manifest", required=True, type=Path)
    evaluate_parser.add_argument("--receipt", required=True, type=Path)
    evaluate_parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    try:
        result = prepare() if args.command == "prepare" else evaluate(read_json(args.manifest), read_json(args.receipt))
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    except (ValueError, OSError) as error:
        parser.exit(2, f"{error}\n")


if __name__ == "__main__":
    main()
