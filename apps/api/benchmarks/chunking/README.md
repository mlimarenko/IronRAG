# Canonical chunking pilot

These opt-in entrypoints compare treatments over one frozen native preparation.
They do not change production templates or select a winner automatically.

## Inputs and order

1. `make backend-bench-chunking-input` validates the pinned source/native responses
   and exports canonical normalized text and blocks. Set
   `CHUNKING_COMPARISON_INPUT`, its `_SHA256`, and a new
   `CHUNKING_CANONICAL_EXPORT` path. The input manifest binds the tested source SHA.
2. Before retrieval scoring, bind every frozen question/checklist fact to exact
   normalized source spans. Preserve the first manifest, annotations and profile
   baseline; corrected preparation has a separate revision and artifact identity.
3. Run `semantic_boundaries.py` in a fresh network-denied CPU process using the
   hash-pinned local Python environment. It verifies that environment through the
   existing semantic-rerank runtime owner. It never restarts an old server or
   paid scope. The model is BGE-M3's trained dense encoder, not a reranker score
   relabelled as an embedding. No questions or labels enter its input.
4. `make backend-bench-chunking-treatments` consumes
   `CHUNKING_CANONICAL_INPUT` and `CHUNKING_SEMANTIC_BOUNDARIES`, each with
   `_SHA256`, and writes a new `CHUNKING_TREATMENT_EXPORT`.
5. `make backend-bench-chunking-retrieval` consumes that export and
   `CHUNKING_QUERY_ANNOTATIONS`, each with `_SHA256`. It requires an explicit
   `CHUNKING_RETRIEVAL_DSN` identifying a **nonexistent** loopback-only
   `irg256_<name>` database on a non-5432 port. It rejects credentials, ambient
   `PG*` settings, existing databases and nonempty schemas. It creates the database
   once, runs ordinary migrations and publishes only the declared fixture.
   `CHUNKING_RETRIEVAL_EXPORT` is a new JSONL journal; failed observations remain.

An optional `CHUNKING_RETRIEVAL_ARM=treatmentId/profileId` selects exactly one
pinned arm. Use a fresh database per arm to compare actual physical table/index
allocation. Without selection, storage is measured for the whole multi-library
experiment and cannot be attributed to individual arms. Per-arm `pg_column_size`
is a row-datum measurement, not physical index bytes. Empty-index migration cost
and incremental FTS/index maintenance during seeding are reported separately.

## Treatments

- **Structural:** the unchanged production builder, including its whole-block
  overlap and context behavior. Oversized atomic/prose blocks remain visible.
- **Recursive:** paragraph, newline, sentence, word and character hierarchy for
  oversized prose, then bounded grouping and whole-fragment overlap.
- **Page:** hard native PDF-page boundaries, with the same recursive oversized
  prose budget primitive. It is PDF-only; other documents remain same-profile
  structural distractors in its search library.
- **Semantic:** query-blind adjacent canonical-block dense-cosine distance,
  with a fixed per-document empirical 90th-percentile break policy. Boundaries
  are frozen before scoring; the same recursive budget primitive is used inside
  segments. This is one explicit paragraph/block-granularity semantic algorithm,
  not a claim to cover every semantic-chunking design.

Alternatives reuse the production block filter, chunk renderer, quality and
context postpasses. Tables, code and records stay atomic. Page/semantic and
atomic boundaries block overlap and neighboring context; structural behavior is
left unchanged. Report those differences rather than attributing every observed
change solely to block size. Derived prose fragments retain exact normalized
character spans; unprovable mappings are refused.

## Measurements and limits

Native retrieval calls the actual `PgSearchStore::search_chunks` with unchanged
questions, top 10, one warmup and three measured calls. Top-5 metrics use that
prefix. All four corpus documents remain present as distractors. Do not compare
the PDF-only arm as a global four-category treatment.

Score exact evidence-span coverage separately from document hits, ranking and
generated answer correctness. A support-table parent ID alone does not make an
entire table available in a row chunk. Context windows are measured separately
from core text. Whitespace counts are not billing tokens. Record actual tokenizer
counts if generation runs, with whole-chunk context admission rather than cutting
conditions or cells to fit.

`generate_answers.py` is a separate first-observation local CPU run using a pinned
Qwen3-0.6B checkpoint and the same verified Python environment. It consumes
`irg256-answer-input/1`, containing only frozen questions and retrieved excerpts,
not checklist answers. It uses non-thinking sampling with matched per-query
seeds, at most 4,096 input tokens and 256 generated tokens, and writes every
answer before proceeding. It never retries, selects best-of outputs or scores
itself. Report output-cap termination and omitted context suffixes. Correctness
needs independent checklist assessment; this small local model is not evidence
of production answer behavior.

All runners refuse network sockets before model loading and use local files only.
Model/package hashes, source SHA, corpus/query pins, settings and first failures
belong in the run receipt. Hosted inference requires a separate explicit admission;
none of these entrypoints dispatches a provider. Hardware costs and training-data
rights are not established by a local zero-provider run.
