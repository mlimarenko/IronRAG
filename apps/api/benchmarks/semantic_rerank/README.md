# Semantic rerank comparison

The Rust benchmark owner exports native effective inputs and computes ranking
metrics. `run_local_comparison.py` supplies scores from two pinned local models.
It does not implement a production reranker or choose a replacement binding.

## Corpus and labels

`corpus.json` contains 24 English queries with 12 candidates each. The stored
order is a seeded shuffle, not a captured production retrieval order. Grades
describe source/fragment relationships; grade 3 uses English query-term overlap.
They are relevance proxies, not independent human judgements.

`query_variants_ru.json` adds authored Russian translations with the same parent
candidates, order and grades. This measures Russian queries over predominantly
English documents. It does not establish Russian-document retrieval quality;
grade 3 remains inherited from the English proxy.

Metrics retain the existing contract: nDCG@5 uses gain `2^grade - 1`; MRR and
P@3 treat `grade >= 2` as relevant. Keep per-language denominators and failures.
Do not treat a gain over shuffled `off` as a gain over an incumbent reranker.

## Input boundary

Export through the zero-provider Rust benchmark entrypoint, not by independently
trimming or reconstructing the corpus in Python. The export uses native
preparation and the bounded SystemOne request builder. Its candidate IDs, opaque
indices, order and exact UTF-8 text define the common input.

The local runner accepts `semantic-rerank-comparison-input/1` and a SHA-256 of the
entire export file. Both pinned tokenizers must accept every unchanged query and
candidate under these limits:

- Query: 128 content tokens
- Sequence or query/candidate pair: 2048 tokens, including special tokens
- No additional clipping, instructions or per-arm text rewriting

The script feeds only query/text pairs to model APIs. Evaluation grades and
source labels remain outside model inputs. Scores are imported back into Rust;
Python does not carry a second nDCG/MRR/P@3 implementation.

From the repository root, choose create-new artifact paths and an exclusive
Cargo target directory, then use the offline targets:

```sh
SEMANTIC_RERANK_COMPARISON_SOURCE_SHA="$(git rev-parse HEAD)" \
SEMANTIC_RERANK_COMPARISON_VARIANTS=apps/api/benchmarks/semantic_rerank/query_variants_ru.json \
SEMANTIC_RERANK_COMPARISON_EXPORT=/data/artifacts/rerank/input.json \
  make backend-bench-semantic-rerank-export \
  BACKEND_CARGO_TARGET_DIR=/data/targets/rerank-comparison

SEMANTIC_RERANK_COMPARISON_INPUT=/data/artifacts/rerank/input.json \
SEMANTIC_RERANK_COMPARISON_SCORES=/data/artifacts/rerank/scores.json \
SEMANTIC_RERANK_COMPARISON_REPORT=/data/artifacts/rerank/report.json \
  make backend-bench-semantic-rerank-replay \
  BACKEND_CARGO_TARGET_DIR=/data/targets/rerank-comparison
```

The export identifies its base commit and the actual source-file hashes
separately; a base SHA does not claim that uncommitted benchmark edits were
already in that commit. Replay checks both the query-array identity and the
entire exported-file SHA-256 against the score artifact. Never substitute a
fresh export after an arm has observed its inputs.

## Local runtime

From this benchmark directory, use Python 3.12 and the checked-in CPU `uv.lock`. Keep the environment, model
weights, vector caches and run results outside the repository:

```sh
UV_PROJECT_ENVIRONMENT=/data/artifacts/rerank-env uv sync --locked
UV_PROJECT_ENVIRONMENT=/data/artifacts/rerank-env uv run --locked \
  python -m unittest discover -s . -p 'test_local_comparison.py'
```

Download and checksum local snapshots before entering the network-denied run:

| Arm | Model | Revision | Publisher-declared license |
| --- | --- | --- | --- |
| Cross-encoder | `BAAI/bge-reranker-v2-m3` | `953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e` | Apache-2.0 |
| Late interaction | `BAAI/bge-m3` | `5617a9f61b028005a4858fdac845db406aefb181` | MIT |

Use the selected configs/tokenizers and the reranker's `model.safetensors`.
BGE-M3 requires `pytorch_model.bin`, **both** `colbert_linear.pt` and
`sparse_linear.pt`: the official loader can leave initialized heads when one is
missing. Do not download a floating revision or enable remote model code.

The environment manifest has protocol `semantic-rerank-local-environment/1`:

- `uvLockPath` and `uvLockSha256` identify the actual dependency lock
- `packages` maps installed distribution names to exact versions
- `models` has exactly `cross-encoder` and `late-interaction`; each contains
  `modelId`, `revision`, local `path`, and `files` mapping filenames to SHA-256
- License/provenance fields may accompany those identities

The runner validates the manifest, installed versions, lock and every selected
file before loading. Published model licenses do not establish permission to
send private inputs to hosted providers or audited training-data rights. Local
use here does not redistribute weights.

## Execution and retained evidence

Run models sequentially, FP32, batch 1, with one intra-op and inter-op thread.
The Linux execution sandbox must deny AF_INET and AF_INET6 socket creation;
offline Hub flags alone are not the sandbox. The entrypoint refuses to load a
model if either Internet socket family remains available. A user-systemd service
can enforce `RestrictAddressFamilies=AF_UNIX` and `IPAddressDeny=any`; also bound
CPU, memory and duration for the host being measured.

`run_local_comparison.py --help` lists the required file/manifest hashes and
output arguments. Output directories and result files are create-new. Every
document encoding and query has started/observed records; an operational failure
stops the arm and leaves later queries unrun. Retain that first result rather
than tuning or retrying to improve a score.

The cross-encoder latency includes tokenization, all pair scores and sorting
with a resident model. BGE-M3 uses only its normalized multi-vector MaxSim score,
not sparse/dense blending, and is **ColBERT-style**, not ColBERTv2. Its online
latency includes query encoding, all MaxSim scores and sorting with resident
document vectors. Report model loading, document encoding/serialization, cache
bytes and peak RSS separately. This is not cold end-to-end latency; do not add
component percentiles to manufacture a cold p95.

Local hosted-provider cost is zero; local hardware cost is unmeasured. The live
`backend-bench-semantic-rerank` entrypoint still calls paid APIs and needs its own
financial admission. A local result, a historical provider aggregate or an
existing credential does not authorize that run.
