# Benchmark Corpus

Grounded QA benchmark for measuring ingestion quality, retrieval accuracy, and graph extraction.

## Corpus

```
corpus/
  wikipedia/     16 files   Wikipedia articles + Wikimedia images (md, png, jpg)
  docs/          30 files   FastAPI, Kubernetes, protocols, databases (md)
  code/          13 files   Go, Rust, Python, TypeScript, Terraform, YAML, JSON
  documents/      5 files   Generated PDF, DOCX, PPTX with structured content
  fixtures/       8 files   Synthetic smoke-test files for upload path validation
```

72 files total. Formats: md, rs, py, go, ts, yaml, json, docx, pptx, pdf, png, jpg.

## Suites

| Suite | Cases | What it tests |
|-------|-------|---------------|
| `api_baseline_suite` | 12 | Single-doc recall from Wikipedia |
| `workflow_strict_suite` | 8 | Cross-doc grounded QA |
| `layout_noise_suite` | 6 | Noisy layout extraction |
| `graph_multihop_suite` | 8 | Graph traversal (non-blocking) |
| `multiformat_surface_suite` | 6 | PDF/DOCX/PPTX/PNG upload smoke |
| `golden_programming_suite` | 16 | FastAPI, Rust, Python, configs |
| `golden_infrastructure_suite` | 8 | Kubernetes, Docker, Terraform |
| `golden_protocols_suite` | 6 | HTTP/2, WebSocket, OAuth, JWT |
| `golden_code_suite` | 20 | Large code file comprehension |
| `golden_multiformat_suite` | 12 | PDF/DOCX/PPTX content extraction |

102 cases total.

## Usage

```bash
export IRONRAG_SESSION_COOKIE="..."
export IRONRAG_BENCHMARK_WORKSPACE_ID="..."

make benchmark-grounded-seed    # upload Wikipedia corpus
make benchmark-grounded-all     # run Wikipedia QA matrix
make benchmark-golden           # upload + run golden dataset (all 5 golden suites)
```

## Tools

| Script | Purpose |
|--------|---------|
| `run_live_benchmark.py` | Upload corpus, wait for ingestion, run QA, evaluate |
| `compare_benchmarks.py` | Side-by-side comparison of two result sets |

Results go to `--output-dir` (default `tmp-grounded-benchmarks/`).
