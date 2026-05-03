<p align="center">
  <img src="./docs/assets/ironrag-logo.svg" alt="IronRAG logo" width="180">
</p>

<p align="center">
  <img src="./docs/assets/readme-flow.gif" alt="IronRAG demo: dashboard, documents, grounded assistant, and graph exploration">
</p>

<h1 align="center">IronRAG</h1>
<p align="center">Production-grade knowledge memory for AI agents and teams</p>

<p align="center">
  <a href="https://github.com/mlimarenko/IronRAG/stargazers"><img src="https://img.shields.io/github/stars/mlimarenko/IronRAG?style=flat-square" alt="Stars"></a>
  <a href="https://github.com/mlimarenko/IronRAG/releases"><img src="https://img.shields.io/github/v/release/mlimarenko/IronRAG?style=flat-square" alt="Release"></a>
  <a href="https://hub.docker.com/r/pipingspace/ironrag-backend"><img src="https://img.shields.io/docker/pulls/pipingspace/ironrag-backend?style=flat-square" alt="Docker Pulls"></a>
  <a href="./LICENSE"><img src="https://img.shields.io/github/license/mlimarenko/IronRAG?style=flat-square" alt="License"></a>
</p>

<p align="center">
  <a href="./docs/en/README.md">English docs</a> &bull;
  <a href="./docs/ru/README.md">Документация</a> &bull;
  <a href="./docs/en/MCP.md">MCP</a> &bull;
  <a href="./docs/en/CLI.md">CLI</a> &bull;
  <a href="./docs/en/IAM.md">IAM</a>
</p>

---

IronRAG turns documents, code, PDFs, spreadsheets, and web pages into a structured knowledge base with a typed knowledge graph. AI agents query it over MCP; humans use the built-in UI. One self-hosted system -- your data stays on your infrastructure.

### Why IronRAG

- **Knowledge graph, not just vectors.** Entities, typed relationships, evidence chains, and document links -- agents reason over structure, not noisy similarity hits.
- **MCP server out of the box.** 21 tools for search, document reading, graph traversal, and web ingestion. Connect Claude, Cursor, VS Code, or any MCP client in one line.
- **Any provider.** OpenAI, DeepSeek, Qwen, or Ollama for fully local inference. Mix freely -- DeepSeek for reasoning, OpenAI for embeddings, Ollama for air-gapped environments.
- **Cost tracking.** Per-document extraction cost and per-query execution cost. Workspace-level price overrides.
- **Fine-grained IAM.** Scoped tokens at system, workspace, or library level. Permission groups control who reads, writes, or connects agents.
- **Code-aware.** 15-language AST parsing via tree-sitter. Config parsers for JSON, YAML, TOML. Technical fact extraction for endpoints, env vars, error codes.
- **CPU-first document recognition.** The backend image includes a Docling CPU runtime for PDF, document-layout Office files, and default raster-image OCR. Spreadsheets use the native tabular parser. No GPU is required; raster-image OCR can be switched per library to an active Vision binding.
- **Scales.** Tested on 5000+ documents, 25k+ graph nodes, 82k+ edges. Batched DB operations, streaming exports, memory-aware worker throttling.
- **Full backup/restore.** One-click tar.zst archive. Selective export (data only or with source files). Restore to the same or different deployment.

## Quick start

```bash
# One-line install (Docker required)
curl -fsSL https://raw.githubusercontent.com/mlimarenko/IronRAG/master/install.sh | bash
```

Or from source:

```bash
git clone https://github.com/mlimarenko/IronRAG.git
cd IronRAG/ironrag
cp .env.example .env          # add IRONRAG_OPENAI_API_KEY=sk-...
docker compose up -d
```

Open [http://127.0.0.1:19000](http://127.0.0.1:19000), create an admin account, upload a document, ask a question.

For fully local inference without cloud providers -- configure Ollama in Admin > AI.

### Other deployment options

```bash
# With S3-compatible storage (bundled s4core)
docker compose -f docker-compose-s4.yml up -d

# Local source build for development
docker compose -f docker-compose-local.yml up --build -d
```

Helm (Kubernetes):

```bash
helm upgrade --install ironrag charts/ironrag \
  --namespace ironrag --create-namespace \
  --set-string app.providerSecrets.openaiApiKey="${OPENAI_API_KEY}" \
  --wait --timeout 20m
```

## How it works

### Recognition routing

IronRAG has one extraction pipeline with an explicit recognition policy per
library. Docling is now embedded in the backend image as a CPU runtime, so the
default path works on ordinary servers and VMs without a GPU. Text-like and
spreadsheet files use deterministic native parsers; document-layout formats use
Docling; raster image OCR can stay local on Docling or use the active Vision
binding.

| Source family | Default engine | Configurable route |
|---------------|----------------|--------------------|
| Text, markup, code, CSV/TSV/XLS/XLSX/XLSB/ODS | `native` | no |
| PDF, DOCX, PPTX | `docling` | no |
| Static raster images (`png`, `jpg`, `tiff`, `bmp`, `webp`) | `docling` | `docling` or active `vision` binding |
| Other supported raster images | `vision` | requires active `vision` binding |

New libraries inherit
`IRONRAG_RECOGNITION_DEFAULT_RASTER_IMAGE_ENGINE=docling`. Operators can update
an individual library through
`PUT /v1/catalog/libraries/{libraryId}/recognition-policy` with
`{"rasterImageEngine":"docling"}` or `{"rasterImageEngine":"vision"}`. Invalid
engines and unknown policy fields are rejected; missing Vision bindings fail
loudly instead of falling back to another route. Video files are not part of the
current ingest surface.

### Document processing pipeline

```mermaid
flowchart TD
  classDef entry fill:#eef6ff,stroke:#3b82f6,stroke-width:2px,color:#0f172a
  classDef api fill:#f8fafc,stroke:#64748b,stroke-width:1.5px,color:#0f172a
  classDef worker fill:#ecfdf5,stroke:#10b981,stroke-width:2px,color:#052e16
  classDef db fill:#fff7ed,stroke:#f97316,stroke-width:2px,color:#431407
  classDef decision fill:#f5f3ff,stroke:#7c3aed,stroke-width:2px,color:#2e1065
  classDef metric fill:#fef9c3,stroke:#eab308,stroke-width:1.5px,color:#422006
  classDef fail fill:#fee2e2,stroke:#ef4444,stroke-width:1.5px,color:#450a0a

  Upload["UI / API upload<br/>file, metadata, libraryId"]:::entry
  Admission["Admission guard<br/>size, MIME, extension, policy<br/>metrics: accepted, rejected"]:::api
  Storage["Source storage<br/>filesystem or object storage<br/>metrics: bytes, checksum"]:::db
  Revision["Postgres revision row<br/>knowledge_revision=pending<br/>metric: revision_id"]:::db
  Operation["Async operation<br/>operation_id, stage=pending"]:::api
  Worker["Worker runtime<br/>bounded retries and budgets<br/>metric: queue_wait_ms"]:::worker

  Detect{"File kind + library recognition policy"}:::decision
  Native["native parsers<br/>text, markdown, HTML, code, spreadsheets<br/>metrics: parser_ms, chars"]:::worker
  DoclingDocs["Docling CPU document layout<br/>PDF, DOCX, PPTX<br/>metrics: extract_ms, pages, tables"]:::worker
  RasterPolicy{"Static raster image?<br/>rasterImageEngine"}:::decision
  DoclingImage["Docling CPU OCR<br/>PNG/JPG/TIFF/BMP/WEBP default<br/>metrics: ocr_ms, chars"]:::worker
  VisionImage["Vision OCR binding<br/>cloud or local provider binding<br/>metrics: provider, model, cost"]:::worker
  VisionOnly["Vision-only image route<br/>formats outside Docling image route<br/>requires active Vision binding"]:::worker
  RecognitionMap["source_map.recognition<br/>engine, capability, structure_tier"]:::metric
  MissingVision["fail loud<br/>missing Vision binding<br/>no silent fallback"]:::fail
  Unsupported["fail loud<br/>unsupported video or binary<br/>no ingest branch"]:::fail

  Normalize["normalize and repair layout<br/>technical text cleanup<br/>metrics: normalized_chars, warnings"]:::worker
  Chunk["chunk_content<br/>semantic blocks and windows<br/>metrics: chunk_count, avg_chunk_chars"]:::worker
  Prepare["structured preparation<br/>headings, tables, block ids<br/>metric: structured_block_count"]:::worker
  Embed["embed_chunk<br/>provider binding<br/>metrics: embedding_dims, embedded_chunks, cost"]:::worker
  Facts["extract_technical_facts<br/>paths, params, endpoints, config<br/>metric: fact_count"]:::worker
  Graph["extract_graph<br/>nodes, edges, evidence<br/>metrics: node_count, edge_count"]:::worker
  Finalize["finalizing<br/>revision ready, vector_state=ready<br/>metric: total_ingest_ms"]:::worker

  Arango["ArangoDB<br/>documents, chunks, vectors,<br/>structured blocks, facts, graph"]:::db
  Postgres["Postgres<br/>catalog, revisions,<br/>operations, accounting"]:::db
  Redis["Redis<br/>graph topology cache<br/>and cache invalidation"]:::db

  Projection["Projection bump<br/>library projection_version++<br/>metric: graph_freshness up to 10s"]:::metric
  Prewarm["Graph prewarm<br/>/graph cache rebuild<br/>metrics: cache_hit < 200ms, miss < 3s"]:::metric
  Ready["Document ready<br/>searchable by lexical, vector,<br/>graph, and technical facts"]:::entry
  Failure["Failure path<br/>stage error, retry exhausted,<br/>visible operation error"]:::fail

  Upload --> Admission --> Storage --> Revision --> Operation --> Worker --> Detect
  Detect -->|"text-like / code / spreadsheets"| Native
  Detect -->|"PDF / DOCX / PPTX"| DoclingDocs
  Detect -->|"PNG / JPG / TIFF / BMP / WEBP"| RasterPolicy
  RasterPolicy -->|"docling (default)"| DoclingImage
  RasterPolicy -->|"vision"| VisionImage
  Detect -->|"GIF / other supported image"| VisionOnly
  Detect -->|"video / unsupported binary"| Unsupported
  VisionImage -. missing binding .-> MissingVision
  VisionOnly -. missing binding .-> MissingVision

  Native --> RecognitionMap
  DoclingDocs --> RecognitionMap
  DoclingImage --> RecognitionMap
  VisionImage --> RecognitionMap
  VisionOnly --> RecognitionMap
  RecognitionMap --> Normalize --> Chunk --> Prepare
  Prepare --> Embed
  Prepare --> Facts
  Prepare --> Graph
  Chunk --> Arango
  Embed --> Arango
  Facts --> Arango
  Graph --> Arango
  Extract --> Postgres
  Embed --> Finalize
  Facts --> Finalize
  Graph --> Finalize
  Finalize --> Postgres
  Finalize --> Projection --> Redis --> Prewarm --> Ready
  Arango --> Prewarm
  Unsupported --> Failure
  MissingVision --> Failure
  Worker -. any stage error .-> Failure
```

### Grounded query pipeline

```mermaid
flowchart LR
  classDef entry fill:#eef6ff,stroke:#2563eb,stroke-width:2px,color:#0f172a
  classDef runtime fill:#f8fafc,stroke:#64748b,stroke-width:1.5px,color:#0f172a
  classDef retrieve fill:#ecfdf5,stroke:#059669,stroke-width:2px,color:#052e16
  classDef db fill:#fff7ed,stroke:#f97316,stroke-width:2px,color:#431407
  classDef answer fill:#f5f3ff,stroke:#7c3aed,stroke-width:2px,color:#2e1065
  classDef metric fill:#fef9c3,stroke:#ca8a04,stroke-width:1.5px,color:#422006
  classDef fail fill:#fee2e2,stroke:#dc2626,stroke-width:1.5px,color:#450a0a

  Ask["UI Assistant / MCP grounded answer<br/>question, libraryId, session"]:::entry
  Auth["Auth and library access<br/>metric: 401 / 403 / 200"]:::runtime
  Execution["Create query_execution<br/>runtimeExecutionId, query_id<br/>metric: trace completeness"]:::runtime
  Context["Conversation context<br/>follow-up rewrite, history focus<br/>metric: effective_query_text"]:::runtime
  IR["Query compiler IR<br/>act, scope, target types<br/>metrics: compile_ms, cache_hit"]:::runtime

  Postgres["Postgres<br/>sessions, executions,<br/>runtime stages, catalog"]:::db
  Redis["Redis<br/>IR cache, graph cache,<br/>answer-context cache"]:::db
  Arango["ArangoDB<br/>chunks, vectors, facts,<br/>graph nodes and edges"]:::db

  Vector["Vector lane<br/>ANN over chunk embeddings<br/>metrics: raw_hit_count > 0, vector_ms < 2s"]:::retrieve
  Lexical["Lexical and title lane<br/>BM25, title matches, topic families<br/>metrics: lexical_ms < 2s, distinct_docs"]:::retrieve
  Entity["Graph/entity lane<br/>person and entity evidence<br/>metric: evidence_chunk_count"]:::retrieve
  Literals["Technical literals lane<br/>paths, INI sections, params<br/>metric: literal_chunk_count"]:::retrieve

  Merge["Merge and diversify<br/>dedupe chunks and documents<br/>metric: retrieved_document_count"]:::retrieve
  Prune["Consolidate / topical prune<br/>single-doc focus or broad topic<br/>metrics: kept_chunks, removed_chunks"]:::retrieve
  Bundle["Context bundle assembly<br/>persist chunk refs and prepared refs<br/>metric: answer_context_chars"]:::retrieve

  Route["Answer router<br/>answer vs clarification<br/>metrics: disposition, variant_count"]:::answer
  Clarify["Clarification response<br/>lists variants, providers, documents<br/>metric: variant_count >= expected"]:::answer
  Generate["Grounded answer generation<br/>selected provider binding<br/>metrics: answer_ms, usage, model"]:::answer
  Verify["Verifier<br/>strict, moderate, lenient<br/>metric: verificationState"]:::answer
  Sources["Source links and evidence detail<br/>preparedSegmentReferences<br/>metrics: citations, document titles"]:::answer
  Response["Grounded response<br/>same quality target for UI and MCP<br/>metric: total_ms p95 <= 30s"]:::entry

  Fail["Fail loud<br/>missing binding 409/422,<br/>provider failure, no silent fallback"]:::fail

  Ask --> Auth --> Execution --> Context --> IR
  Execution --> Postgres
  IR <--> Redis
  IR --> Vector
  IR --> Lexical
  IR --> Entity
  IR --> Literals
  Vector <--> Arango
  Lexical <--> Arango
  Entity <--> Arango
  Literals <--> Arango
  Vector --> Merge
  Lexical --> Merge
  Entity --> Merge
  Literals --> Merge
  Merge --> Prune --> Bundle
  Bundle --> Postgres
  Bundle --> Arango
  Bundle --> Route
  Route -->|broad ambiguous topic| Clarify --> Sources --> Response
  Route -->|focused grounded query| Generate --> Verify --> Sources --> Response
  Generate -. provider or binding error .-> Fail
  Verify -. unsupported or conflicting answer .-> Fail
```

## Tech stack

| Layer | Technology |
|-------|-----------|
| Backend | Rust, Axum, tokio, SQLx |
| Frontend | React, Vite, TypeScript, Tailwind, shadcn/ui |
| Graph rendering | Sigma.js + Graphology (WebGL, Web Worker layout) |
| Knowledge graph | ArangoDB |
| Document store | PostgreSQL |
| Job queue | Redis |
| Code parsing | tree-sitter (15 languages) |
| Deployment | Docker Compose, Helm |

## MCP tools

21 tools out of the box. Create a token in **Admin > Access**, copy the snippet from **Admin > MCP**.

| Category | Tools |
|----------|-------|
| Documents | `search_documents`, `read_document`, `list_documents`, `upload_documents`, `update_document`, `delete_document` |
| Graph | `search_entities`, `get_graph_topology`, `list_relations` |
| Web crawl | `submit_web_ingest_run`, `get_web_ingest_run`, `cancel_web_ingest_run` |
| Q&A | `ask` (grounded answer in a single call) |
| Discovery | `list_workspaces`, `list_libraries` |

## Documentation

| | English | Russian |
|--|---------|---------|
| Overview | [README](./docs/en/README.md) | [README](./docs/ru/README.md) |
| Pipeline | [PIPELINE](./docs/en/PIPELINE.md) | [PIPELINE](./docs/ru/PIPELINE.md) |
| MCP | [MCP](./docs/en/MCP.md) | [MCP](./docs/ru/MCP.md) |
| IAM | [IAM](./docs/en/IAM.md) | [IAM](./docs/ru/IAM.md) |
| CLI | [CLI](./docs/en/CLI.md) | [CLI](./docs/ru/CLI.md) |

## Star History

<p align="center">
  <a href="https://star-history.com/#mlimarenko/IronRAG&Date">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=mlimarenko/IronRAG&type=Date&theme=dark" />
      <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=mlimarenko/IronRAG&type=Date" />
      <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=mlimarenko/IronRAG&type=Date" width="700" />
    </picture>
  </a>
</p>

## License

[MIT](./LICENSE)
