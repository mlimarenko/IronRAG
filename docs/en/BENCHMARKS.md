# IronRAG benchmarks

IronRAG keeps its grounded-query datasets in `apps/api/benchmarks/grounded_query/`. The benchmark corpus is test data, not operator documentation; the commands and evaluation contract live here.

## Corpus layout

```text
apps/api/benchmarks/grounded_query/
├── corpus/
│   ├── wikipedia/   general knowledge articles
│   ├── docs/        technical docs and contract fixtures
│   ├── code/        code and config files
│   ├── documents/   PDF, DOCX, PPTX fixtures
│   ├── graph/       multi-hop graph topology fixtures
│   └── fixtures/    upload-path smoke fixtures
├── *.json           suite definitions
├── rank_relevance.json
├── run_live_benchmark.py
└── compare_benchmarks.py
```

## Suites

| Suite | Purpose |
|---|---|
| `api_baseline_suite` | single-document retrieval quality |
| `workflow_strict_suite` | multi-document grounded QA |
| `layout_noise_suite` | extraction robustness on noisy layouts |
| `graph_multihop_suite` | graph-backed traversal quality |
| `multiformat_surface_suite` | multi-format upload and extraction |
| `technical_contract_suite` | exact technical literals: endpoints, parameters, absent capabilities, transport comparisons |
| `golden_*_suite` | broader programming, infrastructure, protocol, code, and multi-format coverage |

`technical_contract_suite` is the exact-literal quality gate. Run it whenever query retrieval, grounding, MCP search/read behavior, or answer assembly changes.

## Running the benchmarks

```bash
export IRONRAG_SESSION_COOKIE="..."
export IRONRAG_BENCHMARK_WORKSPACE_ID="..."
export IRONRAG_BENCHMARK_RUNTIME_ARTIFACT_DIGEST="sha256:<64 lowercase hex>"

make benchmark-grounded-seed
make benchmark-grounded-all
make benchmark-grounded-technical
make benchmark-golden
```

The session cookie is accepted only through `IRONRAG_SESSION_COOKIE` or a
pre-provisioned file named by `IRONRAG_SESSION_COOKIE_FILE`; command-line cookie
arguments are rejected so the secret cannot leak through the process list.

`make benchmark-grounded` uses the `IRONRAG_BENCHMARK_SUITES` matrix and writes
to `tmp-grounded-benchmarks/`. `make benchmark-golden` switches to the broader
`golden_*_suite` matrix and writes to `tmp-golden-benchmarks/`.

For release candidates, pair the public suites with a private live smoke on
representative operator data. Keep the private prompts, document labels, and
expected strings outside git; publish only sanitized aggregate evidence such as
HTTP status, lifecycle state, verifier state, answer length, source count,
matched structural markers, and whether forbidden generic markers were absent.
Setup/procedure changes should include at least:

- one broad multi-variant setup request that must answer instead of clarifying;
- one focused setup request that must stay on the focused evidence path;
- one versioned procedure request that must retrieve a transition
  procedure instead of a adjacent transition or compatibility page;
- one application-update procedure request that must expose an ordered
  grounded sequence.

## Direct scripts

```bash
python3 apps/api/benchmarks/grounded_query/run_live_benchmark.py --help
python3 apps/api/benchmarks/grounded_query/compare_benchmarks.py baseline-dir candidate-dir
```

`compare_benchmarks.py` accepts two result directories and is a fail-closed
regression gate. It rejects a previously passing case that fails, missing paired
cases or latency samples, lower labelled MRR/hit metrics, or a p50/p95/p99
increase above 10% by default. It also requires every candidate strict case to
pass and enforces absolute grounded-answer ceilings of p50 <= 12 s and
p95 <= 30 s. The budgets are configurable with
`--max-latency-regression-percent`, `--max-candidate-p50-ms`, and
`--max-candidate-p95-ms`; `--json-output` writes the machine-readable decision.

Latency percentiles use only identical `suiteId/caseId` pairs from the baseline
and candidate. Added cases cannot replace a missing baseline sample or change
the regression percentiles. p50/p95/p99 use the conservative nearest-rank
definition (no interpolation toward a lower sample).

Every result also carries SHA-256 fingerprints for the complete case
definition, suite definition, ordered corpus bytes read back from the running
service, and matrix runtime knobs (`queryTopK`, cache policy, round id, isolated
session policy, and corpus reuse mode). Comparison fails closed when a
fingerprint is missing or differs. Therefore changing a question, expected literal,
relevance label, threshold, fixture byte, suite order, or top-k cannot be
mistaken for a product improvement. Baselines created before this integrity
contract must be rerun.

The equivalent Make target is:

```bash
make benchmark-regression \
  IRONRAG_BENCHMARK_BASELINE_DIR=results/baseline \
  IRONRAG_BENCHMARK_CANDIDATE_DIR=results/candidate
```

The runner creates a fresh query session per independent case and executes the
timed answer before the auxiliary rank-search probe, so earlier turns and rank
cache warming cannot bias answer latency. Reused libraries are accepted only
when the exact primary-document inventory and original source bytes match the
local fixtures.

Capture baseline and candidate in at least three alternating paired rounds. Set
`IRONRAG_BENCHMARK_RUNTIME_LABEL`, the required immutable
`IRONRAG_BENCHMARK_RUNTIME_ARTIFACT_DIGEST`, one of the explicit `cold`, `warm`,
or `mixed` cache policies, and `IRONRAG_BENCHMARK_ROUND_ID`. A round-keyed
SHA-256 permutation changes case order between rounds while giving the paired
baseline and candidate the same order. The comparator requires non-empty,
distinct artifact digests and the same cache policy, round, and host-environment
fingerprint.

Release eligibility is recomputed from raw pre/mid/post snapshots instead of
trusting the stored boolean. Every snapshot must pass load-per-CPU, available
memory, used swap, and CPU/memory/I/O PSI gates; the host policy cannot weaken
the repository defaults. A busy-host override or a missing/mismatched hardware,
kernel, boot, cgroup, CPU, or memory identity makes the run diagnostic evidence
only.

Latency policy has three explicit levels: grounded answers target p50 <= 12 s
and p95 <= 30 s; a complete agent turn has a hard p95 ceiling of 90 s; rollout
canaries intentionally apply the stricter 25 s target. Relative regression
limits still apply even when an absolute ceiling passes.

The legacy `scripts/bench/compare_pg_vs_baseline.py` command delegates grounded
decisions to the same canonical comparator. Its combined release verdict also
requires valid baseline and candidate `agent_turn_p95.result.json` artifacts,
the candidate agent quality gate, agent p95 <= 90 s, and <= 10% relative agent
p95 regression.

## Result contract

Benchmark runs write to `tmp-grounded-benchmarks/` by default and include:

- per-case pass/fail details,
- per-case, suite, corpus, and matrix integrity fingerprints,
- runtime version/label and required immutable artifact digest,
- constrained cache policy, paired round id, deterministic case order, and
  compatible environment identity,
- independently evaluated host load, memory, swap, and PSI snapshots at
  pre/mid/post phases,
- `failedChecks` for each broken assertion,
- suite-level `failureReasonCounts`,
- suite-level and matrix-level `summary.rankMetrics`,
- suite-level and matrix-level `summary.answerLatencyMs` with sample count and
  p50/p95/p99,
- appended `rank_metrics_trend.jsonl` records in the output directory,
- latency and evidence metadata for each case.

The goal is not only pass/fail. The output should tell you whether a drop came from retrieval, answer assembly, evidence selection, or verification.

## Retrieval rank metrics

For cases with known relevance, the live runner records ordered retrieval
quality separately from answer correctness:

- document rank metrics use the expected document checks from each suite unless
  `rank_relevance.json` overrides them with explicit `relevantDocuments`;
- chunk rank metrics use optional `relevantChunks` marker strings matched
  against retrieved chunk text;
- each metric family reports `hit@1`, `hit@3`, `hit@5`, `hit@10`, `MRR`, and
  `caseCount`.

The relevance data is synthetic fixture data only. Private or operator-specific
corpora should stay outside this repository; publish only sanitized aggregate
evidence.

`searchQuery` is the exact input sent to the question-agnostic search endpoint.
When a document-rank expectation depends on a public subject disambiguator from
the benchmark question, keep that disambiguator in `searchQuery` instead of
expecting search to infer it.

## Large-document ingest smoke

Large private ingest corpora are not stored in this public repository. When
validating changes to Docling, chunking, embedding, graph extraction, or worker
leases, run the private large-document smoke and record only sanitized evidence
in public docs:

- all files reached `ready`;
- resumed jobs reused completed PDF page-range units;
- graph topology was non-empty after finalization;
- encoding scanners found no mojibake in persisted graph labels or page units;
- document UI showed stage progress, model, duration, calls, and cost;
- public `make check` passed.
