#!/usr/bin/env python3

from __future__ import annotations

import copy
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

try:
    from . import lexical_experiment as experiment
except ImportError:  # unittest discovery imports this module without a package.
    import lexical_experiment as experiment


def native_receipt(manifest: dict) -> dict:
    documents = {doc["key"]: doc for doc in manifest["documents"]}
    results = []
    for arm in experiment.ARMS:
        for query in manifest["queries"]:
            doc = documents[query["relevant_documents"][0]]
            results.append({"arm": arm, "query_id": query["id"],
                            "samples_ms": list(range(1, 16)),
                            "hits": [hit(doc)]})
    return {"schema_version": 1, "manifest_sha256": manifest["manifest_sha256"],
            "arms": list(experiment.ARMS),
            "versions": {"postgres": "18", "pg_search": "0.26.2", "vector": "test"},
            "settings": {"fixture": "synthetic receipt, no database executed"},
            "setup_ms": {"fixture": 0},
            "index_bytes": {arm: 100 for arm in experiment.ARMS}, "results": results}


def hit(document: dict) -> dict:
    return {"chunk_id": document["chunk_id"], "document_key": document["key"],
            "score": -1.0, "text_sha256": document["sha256"]}


class LexicalExperimentTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.manifest = experiment.prepare()
        cls.receipt = native_receipt(cls.manifest)

    def test_prepare_is_deterministic_and_freezes_exact_file_bytes(self) -> None:
        self.assertEqual(self.manifest, experiment.prepare())
        self.assertEqual(len(self.manifest["documents"]), 11)
        self.assertEqual(self.manifest["manifest_sha256"], experiment.manifest_digest(self.manifest))
        for doc in self.manifest["documents"]:
            data = (experiment.ROOT / doc["path"]).read_bytes()
            self.assertEqual(data, doc["text"].encode("utf-8"))
            self.assertEqual(experiment.sha256(data), doc["sha256"])
        self.assertEqual({Path(row["path"]).suffix for row in self.manifest["excluded_fixtures"]},
                         {".pdf", ".png"})

    def test_cases_preserve_suite_queries_and_resolve_all_relevance(self) -> None:
        paths, cases, _ = experiment.load_suite(experiment.ROOT / "golden_hardening_suite.json")
        selected = {doc["key"] for doc in self.manifest["documents"]}
        prepared = {query["id"]: query for query in self.manifest["queries"]}
        skipped = {row["id"] for row in self.manifest["settings"]["excluded_cases"]}
        for case in cases:
            expected = {path.name for path in paths
                        if any(marker in path.name for marker in case.expected_documents_contains + case.relevant_documents)}
            if expected <= selected and case.relevant_documents:
                query = prepared[case.case_id]
                self.assertEqual(query["query"], case.search_query)
                relevant = {path.name for path in paths if any(marker in path.name for marker in case.relevant_documents)}
                self.assertEqual(set(query["relevant_documents"]), relevant)
                self.assertEqual(query["bm25_query"], experiment.ordinary_bm25(case.search_query))
            else:
                self.assertIn(case.case_id, skipped)
                self.assertNotIn(case.case_id, prepared)

    def test_token_escaping_and_ambiguous_intent(self) -> None:
        self.assertEqual(experiment.quote_token('a"b\\c'), '"a\\"b\\\\c"')
        self.assertEqual(experiment.ordinary_bm25("a b-c"), '"a" AND "b-c"')
        for query in ('"a b"', 'a OR b', 'a -b', '(a b)', ''):
            with self.subTest(query=query), self.assertRaises(ValueError):
                experiment.ordinary_bm25(query)

    def test_rank_and_latency_use_canonical_metrics(self) -> None:
        receipt = copy.deepcopy(self.receipt)
        first = receipt["results"][0]
        relevant = first["hits"][0]
        distractor = next(doc for doc in self.manifest["documents"] if doc["key"] != relevant["document_key"])
        first["hits"] = [hit(distractor), relevant]
        output = experiment.evaluate(self.manifest, receipt)
        row = next(row for row in output["arms"][first["arm"]]["queries"] if row["query_id"] == first["query_id"])
        self.assertEqual(row["rank_metrics"], experiment.compute_rank_metrics(
            [distractor["key"], relevant["document_key"]], {relevant["document_key"]}))
        self.assertEqual(row["rank_metrics"]["mrr"], 0.5)
        self.assertFalse(row["rank_metrics"]["hit@1"])
        self.assertTrue(row["rank_metrics"]["hit@3"])
        self.assertEqual(row["latency_ms"], {"median": 8, "p95": 15})
        count = len(self.manifest["queries"])
        self.assertAlmostEqual(output["arms"][first["arm"]]["aggregate_rank_metrics"]["mrr"],
                               (count - 0.5) / count)
        self.assertEqual(output["adoption"], "not decided")

    def test_boolean_controls_are_explicit_and_exclusion_is_separate(self) -> None:
        controls = {query["id"]: query for query in self.manifest["queries"] if query["id"].startswith("control_boolean")}
        documents = {doc["key"]: doc for doc in self.manifest["documents"]}
        for query in controls.values():
            negative = " -" in query["query"]
            self.assertEqual(' AND -"' in query["bm25_query"], negative)
            term = query["query"].split()[-1].lstrip("-")
            for key in query["relevant_documents"]:
                self.assertEqual(term in documents[key]["text"], not negative)
            for key in query["excluded_documents"]:
                self.assertEqual(term in documents[key]["text"], negative)
        clean = experiment.evaluate(self.manifest, self.receipt)
        self.assertEqual(clean["arms"]["raw_bm25"]["exclusion_violation_count"], 0)
        receipt = copy.deepcopy(self.receipt)
        query = controls["control_boolean_not_cumulative"]
        row = next(row for row in receipt["results"] if row["arm"] == "raw_bm25" and row["query_id"] == query["id"])
        row["hits"].append(hit(documents[query["excluded_documents"][0]]))
        output = experiment.evaluate(self.manifest, receipt)["arms"]["raw_bm25"]
        self.assertEqual(output["exclusion_violation_count"], 1)
        measured = next(row for row in output["queries"] if row["query_id"] == query["id"])
        self.assertEqual(measured["rank_metrics"]["mrr"], 1)
        self.assertEqual(measured["excluded_hits"], query["excluded_documents"])

    def test_reject_corrupt_native_receipts(self) -> None:
        mutations = {
            "manifest": lambda r: r.update(manifest_sha256="wrong"),
            "missing arm": lambda r: r["arms"].pop(),
            "duplicate arm": lambda r: r["arms"].append(r["arms"][0]),
            "unknown arm": lambda r: r["arms"].__setitem__(0, "unknown"),
            "missing pair": lambda r: r["results"].pop(),
            "duplicate pair": lambda r: r["results"].append(copy.deepcopy(r["results"][0])),
            "query ID": lambda r: r["results"][0].update(query_id="unknown"),
            "pair arm": lambda r: r["results"][0].update(arm="unknown"),
            "chunk ID": lambda r: r["results"][0]["hits"][0].update(chunk_id="unknown"),
            "document": lambda r: r["results"][0]["hits"][0].update(document_key="wrong"),
            "checksum": lambda r: r["results"][0]["hits"][0].update(text_sha256="wrong"),
            "duplicate raw chunk": lambda r: next(row for row in r["results"] if row["arm"] == "raw_fts")["hits"].extend(
                copy.deepcopy(next(row for row in r["results"] if row["arm"] == "raw_fts")["hits"])),
            "missing sample": lambda r: r["results"][0]["samples_ms"].pop(),
            "extra sample": lambda r: r["results"][0]["samples_ms"].append(1),
            "negative sample": lambda r: r["results"][0]["samples_ms"].__setitem__(0, -1),
            "boolean sample": lambda r: r["results"][0]["samples_ms"].__setitem__(0, True),
            "nan sample": lambda r: r["results"][0]["samples_ms"].__setitem__(0, float("nan")),
            "inf sample": lambda r: r["results"][0]["samples_ms"].__setitem__(0, float("inf")),
            "nan score": lambda r: r["results"][0]["hits"][0].update(score=float("nan")),
            "inf score": lambda r: r["results"][0]["hits"][0].update(score=float("inf")),
            "boolean score": lambda r: r["results"][0]["hits"][0].update(score=True),
            "setup": lambda r: r.update(setup_ms={"bad": -1}),
            "index": lambda r: r.update(index_bytes={"bad": 1.5}),
        }
        for name, mutate in mutations.items():
            with self.subTest(name=name):
                receipt = copy.deepcopy(self.receipt)
                mutate(receipt)
                with self.assertRaises(ValueError):
                    experiment.evaluate(self.manifest, receipt)

    def test_product_duplicates_remain_unscored_reference_anomalies(self) -> None:
        receipt = copy.deepcopy(self.receipt)
        reference = receipt["results"][0]
        repeated = copy.deepcopy(reference["hits"][0])
        repeated["score"] = 0.00886
        reference["hits"].append(repeated)
        output = experiment.evaluate(self.manifest, receipt)
        product = output["arms"]["production_fts"]
        self.assertEqual(product["status"], "invalid_reference")
        self.assertIsNone(product["aggregate_rank_metrics"])
        row = next(row for row in product["queries"] if row["query_id"] == reference["query_id"])
        self.assertIsNone(row["rank_metrics"])
        self.assertEqual(row["duplicate_chunk_ids"], [repeated["chunk_id"]])
        self.assertEqual(receipt["results"][row["native_result_index"]], reference)
        self.assertEqual(len(reference["hits"]), 2)
        self.assertEqual(output["diagnostic_pair_status"], "measured")
        self.assertEqual(output["production_reference_status"], "invalid_reference")
        self.assertEqual(output["status"], "partial_diagnostic")
        self.assertEqual(product["latency_context"], "observed invalid-reference path")
        for arm in ("raw_fts", "raw_bm25"):
            self.assertEqual(output["arms"][arm]["aggregate_rank_metrics"]["mrr"], 1)
            raw = next(row for row in receipt["results"] if row["arm"] == arm)
            raw["hits"].append(copy.deepcopy(raw["hits"][0]))
            with self.assertRaisesRegex(ValueError, "duplicate chunk in diagnostic pair"):
                experiment.evaluate(self.manifest, receipt)
            raw["hits"].pop()
        reference["hits"][1]["text_sha256"] = "wrong"
        with self.assertRaisesRegex(ValueError, "checksum"):
            experiment.evaluate(self.manifest, receipt)

    def test_no_hits_is_valid_and_has_zero_relevance(self) -> None:
        receipt = copy.deepcopy(self.receipt)
        for result in receipt["results"]:
            result["hits"] = []
        for summary in experiment.evaluate(self.manifest, receipt)["arms"].values():
            self.assertEqual(summary["aggregate_rank_metrics"]["mrr"], 0)
            self.assertEqual(summary["exclusion_violation_count"], 0)
            self.assertFalse(any(row["rank_metrics"]["hit@10"] for row in summary["queries"]))

    def test_manifest_tamper_and_invalid_identities(self) -> None:
        for field, value in (("text", "tamper"), ("chunk_id", "not-uuid"), ("key", "wrong.md")):
            with self.subTest(field=field):
                manifest = copy.deepcopy(self.manifest)
                manifest["documents"][0][field] = value
                with self.assertRaises(ValueError):
                    experiment.evaluate(manifest, self.receipt)
                manifest["manifest_sha256"] = experiment.manifest_digest(manifest)
                with self.assertRaises(ValueError):
                    experiment.validate_manifest(manifest)

    def test_cli_prepare_evaluate_and_duplicate_json_keys(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "manifest.json"
            receipt = root / "receipt.json"
            output = root / "evaluation.json"
            commands = [
                ["prepare", "--output", str(manifest)],
                ["evaluate", "--manifest", str(manifest), "--receipt", str(receipt), "--output", str(output)],
            ]
            receipt.write_text(json.dumps(self.receipt), encoding="utf-8")
            for args in commands:
                result = subprocess.run([sys.executable, str(Path(experiment.__file__)), *args],
                                        capture_output=True, text=True, timeout=30, check=False)
                self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(experiment.read_json(manifest), self.manifest)
            self.assertEqual(experiment.read_json(output)["final_answer"], "not measured")
            receipt.write_text('{"arms": [], "arms": []}', encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "duplicate JSON key"):
                experiment.read_json(receipt)
            receipt.write_text('{"score": NaN}', encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "nonfinite JSON"):
                experiment.read_json(receipt)


if __name__ == "__main__":
    unittest.main()
