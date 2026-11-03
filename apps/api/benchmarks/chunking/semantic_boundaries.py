"""Freeze query-blind BGE-M3 dense-cosine boundaries over canonical blocks.

Use the existing hash-pinned CPU environment, in a fresh network-denied local
scope. This is a preparation experiment, not a resumed reranker run or a server.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import math
import os
import resource
import time
from itertools import pairwise
from pathlib import Path


def sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def write_new(path: Path, value: object) -> None:
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2, allow_nan=False)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())


def load_input(path: Path, expected: str) -> dict:
    if path.stat().st_size > 64 * 1024 * 1024 or sha256(path) != expected:
        raise ValueError("canonical input identity or byte bound failed")
    data = json.loads(path.read_text())
    if data["protocol"] != "irg256-native-treatment-input/1":
        raise ValueError("wrong canonical input protocol")
    if not 1 <= len(data["cases"]) <= 16:
        raise ValueError("canonical case bound failed")
    ids = set()
    for case in data["cases"]:
        if case["caseId"] in ids or not 2 <= len(case["blocks"]) <= 512:
            raise ValueError("duplicate case or block count bound failed")
        ids.add(case["caseId"])
        if (
            hashlib.sha256(case["normalizedText"].encode()).hexdigest()
            != case["normalizedSha256"]
        ):
            raise ValueError("normalized source identity failed")
        blocks = case["blocks"]
        if [block["ordinal"] for block in blocks] != list(range(len(blocks))):
            raise ValueError("canonical block ordinals are not contiguous")
    return data


def choose_breaks(
    ordinals: list[int], distances: list[float], quantile: float
) -> list[int]:
    if not 0 < quantile < 1 or len(distances) != len(ordinals) - 1 or not distances:
        raise ValueError("invalid semantic distance envelope")
    if any(not math.isfinite(value) for value in distances):
        raise ValueError("non-finite cosine distance")
    ordered = sorted(distances)
    threshold = ordered[min(len(ordered) - 1, math.ceil(quantile * len(ordered)) - 1)]
    return [
        ordinals[index + 1]
        for index, value in enumerate(distances)
        if value >= threshold
    ]


def runtime_owner():
    path = Path(__file__).parents[1] / "semantic_rerank" / "run_local_comparison.py"
    spec = importlib.util.spec_from_file_location("native_rerank_runtime_owner", path)
    if spec is None or spec.loader is None:
        raise ValueError("pinned runtime verification owner missing")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module, sha256(path)


def run(args: argparse.Namespace) -> None:
    owner, owner_hash = runtime_owner()
    owner.require_network_isolation()
    data = load_input(args.input, args.input_sha256)
    environment = owner.verify_environment(
        args.environment, args.environment_sha256, "late-interaction"
    )
    args.output.mkdir(exist_ok=False)
    journal = args.output / "events.jsonl"

    def record(value: dict) -> None:
        with journal.open("a", encoding="utf-8") as stream:
            stream.write(json.dumps(value, ensure_ascii=False, allow_nan=False) + "\n")
            stream.flush()
            os.fsync(stream.fileno())

    write_new(
        args.output / "admission.json",
        {
            "protocol": "irg256-local-semantic-admission/1",
            "inputSha256": args.input_sha256,
            "environmentSha256": args.environment_sha256,
            "runnerSha256": sha256(Path(__file__)),
            "runtimeOwnerSha256": owner_hash,
            "queryOrLabelInputs": False,
            "providerLiabilityUsd": "0",
            "noAutomaticRetry": True,
            "maxBlocksPerCase": 512,
            "sequenceTokens": 8192,
            "batchSize": 1,
            "threads": 1,
            "quantile": args.quantile,
            "networkFamiliesDenied": True,
        },
    )
    started = time.perf_counter()
    try:
        import numpy as np
        import torch
        from FlagEmbedding import BGEM3FlagModel
        from transformers import AutoTokenizer

        torch.set_num_threads(1)
        torch.set_num_interop_threads(1)
        torch.manual_seed(0)
        model_path = environment["models"]["late-interaction"]["path"]
        tokenizer = AutoTokenizer.from_pretrained(
            model_path, local_files_only=True, trust_remote_code=False
        )
        selected = []
        for case in data["cases"]:
            blocks = [
                block
                for block in case["blocks"]
                if not block["isBoilerplate"] and block["blockKind"] != "table"
            ]
            if len(blocks) < 2:
                raise ValueError("not enough canonical units for semantic boundaries")
            for block in blocks:
                count = len(
                    tokenizer.encode(block["normalizedText"], add_special_tokens=True)
                )
                if not 1 <= count <= 8192:
                    raise ValueError(
                        "canonical unit exceeds token budget; no truncation"
                    )
            selected.append((case, blocks))
        load_started = time.perf_counter()
        model = BGEM3FlagModel(
            model_path,
            devices="cpu",
            use_fp16=False,
            use_bf16=False,
            trust_remote_code=False,
            normalize_embeddings=True,
            batch_size=1,
            passage_max_length=8192,
            return_dense=True,
            return_sparse=False,
            return_colbert_vecs=False,
        )
        owner.preserve_forward_failure(model.model)
        load_ms = (time.perf_counter() - load_started) * 1000
        cases = []
        for case, blocks in selected:
            record({"event": "case_started", "caseId": case["caseId"]})
            vectors = []
            before = time.perf_counter()
            for block in blocks:
                vector = model.encode_corpus(
                    [block["normalizedText"]],
                    batch_size=1,
                    max_length=8192,
                    return_dense=True,
                    return_sparse=False,
                    return_colbert_vecs=False,
                )["dense_vecs"][0]
                if vector.shape != (1024,) or not np.isfinite(vector).all():
                    raise ValueError("invalid BGE-M3 dense vector")
                if not np.isclose(np.linalg.norm(vector), 1, atol=1e-4):
                    raise ValueError("dense vector is not unit normalized")
                vectors.append(vector)
                record(
                    {
                        "event": "block_observed",
                        "caseId": case["caseId"],
                        "ordinal": block["ordinal"],
                        "textSha256": hashlib.sha256(
                            block["normalizedText"].encode()
                        ).hexdigest(),
                    }
                )
            matrix = np.stack(vectors)
            vector_file = args.output / (case["caseId"] + "-dense.npy")
            with vector_file.open("xb") as stream:
                np.save(stream, matrix, allow_pickle=False)
            distances = [
                float(1 - np.dot(left, right)) for left, right in pairwise(matrix)
            ]
            ordinals = [block["ordinal"] for block in blocks]
            cases.append(
                {
                    "caseId": case["caseId"],
                    "normalizedSha256": case["normalizedSha256"],
                    "encodedBlockOrdinals": ordinals,
                    "adjacentCosineDistances": distances,
                    "breakBeforeOrdinals": choose_breaks(
                        ordinals, distances, args.quantile
                    ),
                    "encodingMs": (time.perf_counter() - before) * 1000,
                    "vectorFile": str(vector_file),
                    "vectorSha256": sha256(vector_file),
                    "vectorBytes": matrix.nbytes,
                }
            )
            record({"event": "case_completed", "caseId": case["caseId"]})
        write_new(
            args.output / "boundaries.json",
            {
                "protocol": "irg256-semantic-boundaries/1",
                "canonicalInputSha256": args.input_sha256,
                "sourceSha": data["sourceSha"],
                "modelId": "BAAI/bge-m3",
                "modelRevision": "5617a9f61b028005a4858fdac845db406aefb181",
                "environmentSha256": args.environment_sha256,
                "runnerSha256": sha256(Path(__file__)),
                "method": "Normalized CLS dense embeddings; adjacent canonical-block cosine distance",
                "thresholdPolicy": "Per-document empirical 90th percentile, fixed before inference and labels",
                "quantile": args.quantile,
                "modelLoadMs": load_ms,
                "elapsedMs": (time.perf_counter() - started) * 1000,
                "peakRssKiB": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,
                "cases": cases,
                "providerCalls": 0,
                "providerLiabilityUsd": "0",
                "hardwareCostUsd": None,
                "truncationAllowed": False,
                "generationCapabilityClaimed": False,
            },
        )
    except BaseException as error:
        record(
            {
                "event": "failed",
                "exceptionType": type(error).__name__,
                "message": str(error),
            }
        )
        raise


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", required=True, type=Path)
    parser.add_argument("--input-sha256", required=True)
    parser.add_argument("--environment", required=True, type=Path)
    parser.add_argument("--environment-sha256", required=True)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--quantile", type=float, choices=[0.9], default=0.9)
    run(parser.parse_args())


if __name__ == "__main__":
    main()
