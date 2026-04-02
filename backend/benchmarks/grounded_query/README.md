# Grounded Query Benchmark Corpus

This benchmark package uses a neutral corpus made of:

- English Wikipedia plaintext extracts and related-context dossiers for semantic QA coverage
- Wikimedia Commons images for non-text ingestion coverage
- locally generated multiformat smoke fixtures for PDF, DOCX, PPTX, PNG, and JPG upload coverage

Corpus files:

- `knowledge_graph_wikipedia.md`
- `graph_database_wikipedia.md`
- `vector_database_wikipedia.md`
- `large_language_model_wikipedia.md`
- `rust_programming_language_wikipedia.md`
- `retrieval_augmented_generation_wikipedia.md`
- `optical_character_recognition_wikipedia.md`
- `transformer_deep_learning_wikipedia.md`
- `semantic_web_wikipedia.md`
- `named_entity_recognition_wikipedia.md`
- `information_retrieval_wikipedia.md`
- `question_answering_wikipedia.md`
- `knowledge_graph_diagram_wikipedia.png`
- `rust_logo_wikipedia.png`
- `semantic_web_stack_wikimedia.jpg`
- `ocr_basic_wikimedia.png`
- `upload_smoke_fixture.pdf`
- `upload_smoke_fixture.docx`
- `upload_smoke_fixture.png`
- `runtime_upload_check.pdf`
- `runtime_upload_check.docx`
- `runtime_upload_check.pptx`
- `runtime_upload_check.png`
- `runtime_upload_check.jpg`

Primary sources:

- <https://en.wikipedia.org/wiki/Knowledge_graph>
- <https://en.wikipedia.org/wiki/Graph_database>
- <https://en.wikipedia.org/wiki/Vector_database>
- <https://en.wikipedia.org/wiki/Large_language_model>
- <https://en.wikipedia.org/wiki/Rust_(programming_language)>
- <https://en.wikipedia.org/wiki/Retrieval-augmented_generation>
- <https://en.wikipedia.org/wiki/Optical_character_recognition>
- <https://en.wikipedia.org/wiki/Transformer_(deep_learning_architecture)>
- <https://en.wikipedia.org/wiki/Semantic_Web>
- <https://en.wikipedia.org/wiki/Named-entity_recognition>
- <https://en.wikipedia.org/wiki/Information_retrieval>
- <https://en.wikipedia.org/wiki/Question_answering>
- <https://commons.wikimedia.org/wiki/File:W3c_semantic_web_stack.jpg>
- <https://commons.wikimedia.org/wiki/File:OCRBasic.png>

Local generated fixtures:

- `upload_smoke_fixture.*` and `runtime_upload_check.*` are privacy-safe synthetic files used to exercise non-Markdown ingestion and query paths.

The benchmark matrix is intentionally split into:

- `api_baseline_suite.json`: single-document grounded recall
- `workflow_strict_suite.json`: strict cross-document grounded QA
- `layout_noise_suite.json`: literal-heavy lists and grouped terms
- `graph_multihop_suite.json`: exploratory cross-document suite that stays non-blocking when graph participation is not deterministic on this neutral corpus
- `multiformat_surface_suite.json`: strict upload and extraction checks across PDF, DOCX, PPTX, PNG, and JPG fixtures

## Load the benchmark corpus into a deployed stack

From the repository root:

```bash
cd /home/leader/sources/RustRAG/rustrag
export RUSTRAG_SESSION_COOKIE="..."
export RUSTRAG_BENCHMARK_WORKSPACE_ID="workspace-uuid"
make benchmark-grounded-seed
```

What this does:

- creates a fresh benchmark library unless `RUSTRAG_BENCHMARK_LIBRARY_ID` is provided
- uploads the full corpus referenced by the configured suite matrix
- waits until the library becomes readable and quiet
- writes `tmp-grounded-benchmarks/upload.result.json`
- prints the created or reused `library.id` so you can inspect the data in the UI

Useful variables:

- `RUSTRAG_BENCHMARK_BASE_URL`: deployed API base URL, default `http://127.0.0.1:19000/v1`
- `RUSTRAG_BENCHMARK_WORKSPACE_ID`: workspace UUID where the benchmark library should live
- `RUSTRAG_SESSION_COOKIE`: value of `rustrag_ui_session`
- `RUSTRAG_BENCHMARK_LIBRARY_NAME`: display name for a new seeded library
- `RUSTRAG_BENCHMARK_LIBRARY_ID`: reuse an existing library instead of creating a fresh one
- `RUSTRAG_BENCHMARK_SUITES`: override the suite list if you want to upload only a subset of the corpus

Examples:

```bash
# seed the full corpus into a new library
make benchmark-grounded-seed

# seed only the multiformat fixtures into an existing library
make benchmark-grounded-seed \\
  RUSTRAG_BENCHMARK_LIBRARY_ID="library-uuid" \\
  RUSTRAG_BENCHMARK_SUITES="backend/benchmarks/grounded_query/multiformat_surface_suite.json"
```

After the corpus is loaded, run the full matrix:

```bash
make benchmark-grounded-all
```
