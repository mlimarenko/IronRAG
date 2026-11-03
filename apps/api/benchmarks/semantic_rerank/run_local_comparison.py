"""Score one frozen native export; Rust's benchmark owner computes rank metrics.

Run in the hash-pinned CPU environment, with AF_INET/AF_INET6 denied by the
execution sandbox. Local files are the only model inputs. This entrypoint never
downloads models, calls providers, tunes against labels or replaces query_rerank.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import json
import math
import os
from pathlib import Path
import resource
import socket
import time
from typing import Any

MODEL_IDENTITIES = {
    "cross-encoder": (
        "BAAI/bge-reranker-v2-m3",
        "953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e",
    ),
    "late-interaction": ("BAAI/bge-m3", "5617a9f61b028005a4858fdac845db406aefb181"),
}
MAX_QUERY_TOKENS = 128
MAX_SEQUENCE_TOKENS = 2048


class ModelForwardFailure(Exception):
    """Escape FlagEmbedding's RuntimeError batch backoff with its cause intact."""


def preserve_forward_failure(model: Any) -> None:
    original = model.forward

    def forward(*args: Any, **kwargs: Any) -> Any:
        try:
            return original(*args, **kwargs)
        except RuntimeError as error:
            # With batch size one, the dependency backs off to zero and can
            # replace this exception with tokenizer.pad's unrelated IndexError.
            raise ModelForwardFailure("model forward failed; no batch retry") from error

    model.forward = forward


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(block)
    return hasher.hexdigest()


def write_new(path: Path, value: Any) -> None:
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2, allow_nan=False)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())


def record(path: Path, value: Any) -> None:
    with path.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps(value, ensure_ascii=False, allow_nan=False) + "\n")
        stream.flush()
        os.fsync(stream.fileno())


def require_network_isolation() -> None:
    for family in (socket.AF_INET, socket.AF_INET6):
        try:
            connection = socket.socket(family, socket.SOCK_STREAM)
        except OSError as error:
            if error.errno not in (1, 13, 97):
                raise
        else:
            connection.close()
            raise ValueError(
                "Internet socket families must be denied before loading models"
            )


def load_inputs(path: Path, expected_sha256: str) -> dict[str, Any]:
    if digest(path) != expected_sha256:
        raise ValueError("native export file identity mismatch")
    data = json.loads(path.read_text())
    if data["protocol"] != "semantic-rerank-comparison-input/1":
        raise ValueError("unsupported native export protocol")
    if not data["queries"] or len(data["inputSha256"]) != 64:
        raise ValueError("missing frozen inputs")
    seen = set()
    for query in data["queries"]:
        if query["id"] in seen or not query["query"].strip():
            raise ValueError("duplicate or empty query")
        seen.add(query["id"])
        candidates = query["candidates"]
        ids = [candidate["id"] for candidate in candidates]
        if not ids or len(set(ids)) != len(ids):
            raise ValueError("duplicate or empty candidate set")
        if [candidate["index"] for candidate in candidates] != list(range(len(ids))):
            raise ValueError("native candidate indices are not contiguous")
        if any(not candidate["text"].strip() for candidate in candidates):
            raise ValueError("empty native candidate text")
    return data


def verify_environment(manifest_path: Path, expected: str, arm: str) -> dict[str, Any]:
    if digest(manifest_path) != expected:
        raise ValueError("execution manifest identity mismatch")
    manifest = json.loads(manifest_path.read_text())
    if manifest["protocol"] != "semantic-rerank-local-environment/1":
        raise ValueError("unsupported environment protocol")
    if digest(Path(manifest["uvLockPath"])) != manifest["uvLockSha256"]:
        raise ValueError("runtime lock changed")
    for package, version in manifest["packages"].items():
        if importlib.metadata.version(package) != version:
            raise ValueError(f"installed runtime version mismatch: {package}")
    for name, model in manifest["models"].items():
        if (model["modelId"], model["revision"]) != MODEL_IDENTITIES[name]:
            raise ValueError("unexpected model revision")
        root = Path(model["path"]).resolve(strict=True)
        required = {"config.json", "tokenizer_config.json", "tokenizer.json"}
        required |= (
            {"model.safetensors"}
            if name == "cross-encoder"
            else {"pytorch_model.bin", "colbert_linear.pt", "sparse_linear.pt"}
        )
        if not required.issubset(model["files"]):
            raise ValueError(
                "incomplete local model snapshot, including projection heads"
            )
        for relative, sha256 in model["files"].items():
            file = (root / relative).resolve(strict=True)
            if not file.is_relative_to(root) or digest(file) != sha256:
                raise ValueError("local model file identity mismatch")
    if (
        set(manifest["models"]) != set(MODEL_IDENTITIES)
        or arm not in manifest["models"]
    ):
        raise ValueError(
            "both pinned tokenizers are required for common-input validation"
        )
    return manifest


def validate_token_envelope(
    data: dict[str, Any], tokenizers: dict[str, Any]
) -> dict[str, Any]:
    counts = {}
    for name, tokenizer in tokenizers.items():
        query_max = candidate_max = pair_max = 0
        for query in data["queries"]:
            query_tokens = len(
                tokenizer.encode(query["query"], add_special_tokens=False)
            )
            query_max = max(query_max, query_tokens)
            if query_tokens > MAX_QUERY_TOKENS:
                raise ValueError(
                    "frozen query exceeds the common token envelope; no truncation allowed"
                )
            for candidate in query["candidates"]:
                text = candidate["text"]
                document_tokens = len(tokenizer.encode(text, add_special_tokens=True))
                pair_tokens = len(
                    tokenizer.encode(query["query"], text, add_special_tokens=True)
                )
                candidate_max = max(candidate_max, document_tokens)
                pair_max = max(pair_max, pair_tokens)
                if max(document_tokens, pair_tokens) > MAX_SEQUENCE_TOKENS:
                    raise ValueError(
                        "frozen candidate exceeds the common token envelope; no truncation allowed"
                    )
        counts[name] = {
            "queryMaxContentTokens": query_max,
            "candidateMaxTokensWithSpecials": candidate_max,
            "pairMaxTokensWithSpecials": pair_max,
        }
    return {
        "counts": counts,
        "additionalTextTruncation": False,
        "queryContentTokenLimit": MAX_QUERY_TOKENS,
        "sequenceTokenLimit": MAX_SEQUENCE_TOKENS,
    }


def finite_scores(
    candidates: list[dict[str, Any]], scores: Any
) -> list[dict[str, Any]]:
    if len(candidates) != len(scores):
        raise ValueError("model did not return exactly one score per candidate")
    values = [float(value) for value in scores]
    if not all(math.isfinite(value) for value in values):
        raise ValueError("model returned a nonfinite score")
    return [
        {"id": candidate["id"], "score": score}
        for candidate, score in zip(candidates, values, strict=True)
    ]


def run(args: argparse.Namespace) -> None:
    require_network_isolation()
    for name, value in {
        "HF_HUB_OFFLINE": "1",
        "TRANSFORMERS_OFFLINE": "1",
        "TOKENIZERS_PARALLELISM": "false",
        "OMP_NUM_THREADS": "1",
        "MKL_NUM_THREADS": "1",
        "OPENBLAS_NUM_THREADS": "1",
    }.items():
        os.environ[name] = value
    data = load_inputs(args.input, args.input_sha256)
    manifest = verify_environment(args.environment, args.environment_sha256, args.arm)
    output = args.output.resolve()
    output.mkdir(mode=0o700)
    journal = output / "attempts.jsonl"
    report = {
        "protocol": "semantic-rerank-comparison-scores/1",
        "inputSha256": data["inputSha256"],
        "exportedInputFileSha256": args.input_sha256,
        "arm": args.arm,
        "environmentSha256": args.environment_sha256,
        "modelIdentity": manifest["models"][args.arm],
        "runnerSha256": digest(Path(__file__)),
        "queries": [],
        "providerCostUsd": 0,
        "hardwareCostUsd": None,
        "hardwareCostBasis": "unmeasured local CPU time; zero hosted-provider calls",
        "configuration": {
            "device": "cpu",
            "precision": "float32",
            "batchSize": 1,
            "intraOpThreads": 1,
            "interOpThreads": 1,
            "scoreTies": "native replay preserves frozen candidate order",
        },
    }
    write_new(
        output / "started.json",
        {
            "inputSha256": data["inputSha256"],
            "exportedInputFileSha256": args.input_sha256,
            "arm": args.arm,
            "environmentSha256": args.environment_sha256,
            "runnerSha256": report["runnerSha256"],
            "noAutomaticReplay": True,
        },
    )
    active_query = None
    try:
        import numpy as np
        import torch
        from FlagEmbedding import BGEM3FlagModel, FlagReranker
        from transformers import AutoTokenizer

        torch.set_num_threads(1)
        torch.set_num_interop_threads(1)
        torch.manual_seed(0)
        tokenizers = {
            name: AutoTokenizer.from_pretrained(
                model["path"], local_files_only=True, trust_remote_code=False
            )
            for name, model in manifest["models"].items()
        }
        report["tokenEnvelope"] = validate_token_envelope(data, tokenizers)
        write_new(output / "token-envelope.json", report["tokenEnvelope"])
        load_start = time.perf_counter()
        path = manifest["models"][args.arm]["path"]
        if args.arm == "cross-encoder":
            model = FlagReranker(
                path,
                devices="cpu",
                use_fp16=False,
                trust_remote_code=False,
                query_max_length=MAX_QUERY_TOKENS,
                max_length=MAX_SEQUENCE_TOKENS,
                batch_size=1,
                normalize=False,
            )
            report["latencyScope"] = (
                "resident-model tokenization, all pair scores and sorting; no document cache"
            )
        else:
            model = BGEM3FlagModel(
                path,
                devices="cpu",
                use_fp16=False,
                use_bf16=False,
                trust_remote_code=False,
                batch_size=1,
                query_max_length=MAX_QUERY_TOKENS + 2,
                passage_max_length=MAX_SEQUENCE_TOKENS,
                return_dense=False,
                return_sparse=False,
                return_colbert_vecs=True,
            )
            report["latencyScope"] = (
                "resident-model/resident-document-cache query encoding, MaxSim scores and sorting"
            )
        preserve_forward_failure(model.model)
        report["configuration"]["forwardFailurePolicy"] = (
            "fail fast, preserve first cause"
        )
        report["modelLoadMs"] = (time.perf_counter() - load_start) * 1000
        cache = {}
        if args.arm == "late-interaction":
            cache_root = output / "document-vectors"
            cache_root.mkdir()
            cache_start = time.perf_counter()
            for query in data["queries"]:
                for candidate in query["candidates"]:
                    text = candidate["text"]
                    key = hashlib.sha256(text.encode()).hexdigest()
                    if key in cache:
                        continue
                    record(journal, {"event": "document_started", "textSha256": key})
                    start = time.perf_counter()
                    vector = model.encode_corpus(
                        [text],
                        batch_size=1,
                        max_length=MAX_SEQUENCE_TOKENS,
                        return_dense=False,
                        return_sparse=False,
                        return_colbert_vecs=True,
                    )["colbert_vecs"][0]
                    encode_ms = (time.perf_counter() - start) * 1000
                    if vector.dtype != np.float32 or not np.isfinite(vector).all():
                        raise ValueError("invalid FP32 document vectors")
                    file = cache_root / (key + ".npy")
                    with file.open("xb") as stream:
                        np.save(stream, vector, allow_pickle=False)
                    cache[key] = vector
                    record(
                        journal,
                        {
                            "event": "document_observed",
                            "textSha256": key,
                            "encodingMs": encode_ms,
                            "shape": list(vector.shape),
                            "tensorBytes": vector.nbytes,
                            "fileBytes": file.stat().st_size,
                            "fileSha256": digest(file),
                        },
                    )
            report["documentPrecomputation"] = {
                "uniqueTexts": len(cache),
                "encodeAndSerializeMs": (time.perf_counter() - cache_start) * 1000,
                "tensorBytes": sum(vector.nbytes for vector in cache.values()),
                "serializedBytes": sum(
                    file.stat().st_size for file in cache_root.iterdir()
                ),
                "cacheLoadMs": None,
                "cacheState": "retained in memory from fresh encoding",
                "coldEndToEndLatencyMs": None,
            }
        for query in data["queries"]:
            active_query = query["id"]
            record(journal, {"event": "query_started", "id": query["id"]})
            start = time.perf_counter()
            if args.arm == "cross-encoder":
                scores = model.compute_score(
                    [[query["query"], row["text"]] for row in query["candidates"]],
                    batch_size=1,
                    query_max_length=MAX_QUERY_TOKENS,
                    max_length=MAX_SEQUENCE_TOKENS,
                    normalize=False,
                )
            else:
                query_vector = model.encode_queries(
                    [query["query"]],
                    batch_size=1,
                    max_length=MAX_QUERY_TOKENS + 2,
                    return_dense=False,
                    return_sparse=False,
                    return_colbert_vecs=True,
                )["colbert_vecs"][0]
                scores = [
                    model.colbert_score(
                        query_vector,
                        cache[hashlib.sha256(row["text"].encode()).hexdigest()],
                    )
                    for row in query["candidates"]
                ]
            mapped = finite_scores(query["candidates"], scores)
            sorted(enumerate(mapped), key=lambda pair: (-pair[1]["score"], pair[0]))
            result = {
                "id": query["id"],
                "status": "ok",
                "scores": mapped,
                "latencyMs": (time.perf_counter() - start) * 1000,
            }
            report["queries"].append(result)
            record(journal, {"event": "query_observed", **result})
            active_query = None
        report["completed"] = True
    except Exception as error:
        # Retain the first failure, not a best-of score. Exception text may name
        # local files but never includes provider request/credential bodies.
        report["completed"] = False
        cause = error.__cause__ if isinstance(error, ModelForwardFailure) else error
        report["failure"] = {"type": type(cause).__name__, "message": str(cause)[:2000]}
        if isinstance(error, ModelForwardFailure):
            report["failure"]["boundary"] = (
                "model.forward; dependency batch retry prevented"
            )
        record(journal, {"event": "operational_failure", **report["failure"]})
        if active_query is not None:
            report["queries"].append(
                {
                    "id": active_query,
                    "status": "failed",
                    "scores": [],
                    "latencyMs": None,
                    "failure": report["failure"],
                }
            )
    finally:
        completed = {query["id"] for query in report["queries"]}
        report["queries"].extend(
            {"id": query["id"], "status": "not_run", "scores": [], "latencyMs": None}
            for query in data["queries"]
            if query["id"] not in completed
        )
        report["peakRssBytes"] = (
            resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * 1024
        )
        write_new(output / "scores.json", report)
    if not report["completed"]:
        raise RuntimeError(
            "local comparison stopped; retain first failure and reconcile before any rerun"
        )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arm", required=True, choices=MODEL_IDENTITIES)
    parser.add_argument("--input", required=True, type=Path)
    parser.add_argument("--input-sha256", required=True)
    parser.add_argument("--environment", required=True, type=Path)
    parser.add_argument("--environment-sha256", required=True)
    parser.add_argument("--output", required=True, type=Path)
    run(parser.parse_args())


if __name__ == "__main__":
    main()
