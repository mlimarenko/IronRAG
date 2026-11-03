# Grounded Query Benchmarks

One self-contained benchmark registry. Every suite lives under the
`golden_` prefix, every corpus file is referenced by at least one suite,
and the answer suites run against a live IronRAG API using only the synthetic
fixtures in this directory. The opt-in [isolated lexical replay](lexical_replay/README.md)
uses those fixtures and rank metrics without an API or model call; its results
are not answer-suite matrices.

## Native MCP evidence replay (offline)

`native_surface_evidence.py` scores preserved **real full MCP JSON-RPC exchanges**
against a selected registry case. It makes no transport, provider or repair call.
HTTP `run_turn` and native replay both use `evaluate_execution_detail` for the
required/forbidden/opening matchers, fact/caveat annotations, reference counts,
verification and auxiliary search/rank rubric. Parity means the same declared
rubric, not identical prose, citations or execution IDs.

```sh
python3 apps/api/benchmarks/grounded_query/native_surface_evidence.py \
  --suite apps/api/benchmarks/grounded_query/golden_hardening_suite.json \
  --case aurora_quorum_fixed_size \
  --capture /path/to/preserved-native-case.json \
  --output /path/to/native-case-result.json
```

The capture envelope has exactly these fields:

| Field | Contract |
|---|---|
| `schemaVersion` | `1` |
| `suiteId`, `caseId` | Selected trusted suite and case IDs |
| `caseDefinitionSha256` | `case_definition_sha256(case)` from the loaded suite policy, including preceding turns |
| `workspaceId`, `libraryId` | Observed scope UUIDs, checked against each nested execution |
| `library` | Explicit native library reference used by every request |
| `turns` | Ordered actual exchanges, one for each preceding turn and then the final question |

Each exchange contains exactly `request`, `response`, and `observation`:

- `request` is the raw JSON-RPC `2.0` request with `id`, `method: tools/call`,
  and `params: {name: grounded_answer, arguments: ...}`. The native wire name is
  `grounded_answer`, not the integration's prefixed `ironrag_grounded_answer`.
  Arguments require `library`, the unchanged `query`, and explicit
  `responseProfile: full`; `maxReferences` is invalid for full and is refused.
  `conversationTurns`, `topK` and `includeDebug` are optional within their native
  bounds. No other request or argument fields are accepted.
- `response` is the raw correlated JSON-RPC response, including the full MCP
  `result.content` and `result.structuredContent`. The latter must carry genuine
  `executionDetail`, consistent `answerBody`, completion/readiness/repair fields,
  and any native identity shortcuts must agree with the detail. The text block
  retains the answer plus appended JSON emitted by `ok_tool_result`; it is not
  just the answer. The handler adds `queryLanguage` to structured content after
  constructing that text block. Compact summaries, missing detail, RPC/tool
  errors, clarification and non-final/repair-required results fail closed.
- `observation` contains exactly `timing`, `cost`, and `search`. Use JSON `null`
  for an unavailable observation; do not insert zeros or derived estimates.
  `timing`, when present, is `{source: captured, answerLatencyMs: <number>}`
  with optional `searchLatencyMs`. Durations must be finite and nonnegative;
  a search duration requires actual auxiliary search evidence.
  `cost`, when present, is the existing typed lineage observation:
  `{rootExecutionId, components: [{billing, executionDetail, llmContext}, ...]}`.
  Every component and edge is revalidated, and the root execution and both turns
  must match the native detail. MCP's sanitized reference metadata need not be
  byte-identical to the HTTP cost component. Missing or invalid components cannot become measured totals.
  Unknown/unpriced coverage stays explicit and currencies are never combined.
  `search`, when present, is
  `{workspaceId, libraryId, queryText, responsePayload}` with the observed search
  response for that turn's declared search query. It is an **auxiliary** rank
  probe, not evidence that MCP performed that search internally. Missing search
  reports unavailable retrieval and no ranks, failing strict case pass even if
  the answer rubric passes.

For multi-turn cases, every request must contain the complete chronological
`conversationTurns` observed so far: user `query`, then assistant `answerBody`
(not the MCP text block's appended JSON), repeated for each prior exchange.
The replay rejects omitted, reordered, rewritten or truncated content, duplicate
execution IDs, more than 20 history entries, history content above 1,200
characters, and questions above 4,000 characters. It never truncates history to
fit. Each MCP call may legitimately have a different transient query conversation;
an MCP transport session is not a durable UI query conversation. Failed preceding
expectations remain in the final result even if the final answer passes.

Input is bounded to 16 MiB, 64 JSON nesting levels and 11 actual turns. Duplicate
JSON object keys, transport headers and credential fields are refused. Preserve
the original capture outside git; do not attach credentials or headers. The
result retains `rawNativeEvidence`, per-turn scores, observed/unavailable timing,
cost coverage, and explicit `surface: mcp_grounded_answer` and invocation policy.
Exit `0` means strict rubric pass; exit `1` means failed checks or rejected
evidence. These results are not HTTP code-change/data-lifecycle comparison
matrices and must not be relabelled or fed to `compare_benchmarks.py` as such.
Fixture tests prove source behavior only. A live native quality baseline requires
separately authorized collection of real evidence; replay cannot create it.

## The registry

Cases may declare `expectedClarification: true` or `false` to require the canonical
execution detail's `clarification.required` to match. A mismatch (including a
missing observation) fails `answerPass` and adds `answer_clarification` to
`failedChecks`; omitted or null expectations preserve existing scoring. Declared
expectations enter the case-definition hash. Adding one changes evaluation policy,
not the executed question or model input; preserved captures retain their original
hash and are not relabelled. Native MCP final-answer admission remains unchanged.

| Suite | Purpose |
|---|---|
| golden_programming | Core programming-corpus answer quality |
| golden_infrastructure | Infrastructure-corpus answer quality |
| golden_protocols | Protocol-corpus answer quality |
| golden_code | Source-code corpus: declarations, structure, literals |
| golden_multiformat | Business documents: docx, pdf, xlsx, pptx content QA |
| golden_hardening | Hard classes: numeric facts with units (regex), compound literals, cross-document hops, config comments, JSONL records, Russian corpus, near-twin distractors, wide tables, deep declaration reach, pdf/png extracts |
| golden_api_baseline | Broad API-surface baseline over the mixed corpus |
| golden_workflow_strict | Ordered procedures and strict workflow answers |
| golden_layout_noise | Answers under noisy/laid-out corpus (incl. wikipedia noise) |
| golden_graph_multihop | Graph-lane multi-hop reasoning topologies |
| golden_upload_surface | Upload smoke over every binary format (docx, pdf, pptx, png, jpg) |
| golden_technical_contract | Technical-contract literal fidelity |

The `golden_*_suite.json` files in this directory are the registry. Both the
CLI without `--suite` and Make discover all of them in filename order; an empty
registry is refused. `IRONRAG_GOLDEN_SUITES` and `IRONRAG_BENCHMARK_SUITES` may
select an explicit subset for Make, as repeated `--suite` does for the CLI.

## Corpus layout

- `corpus/docs/` — markdown documents (incl. one Russian runbook)
- `corpus/code/` — source-code fixtures (py, go, rs, yaml, txt, …)
- `corpus/documents/` — binary business documents (docx, pdf, xlsx, pptx)
- `corpus/fixtures/` — upload-surface fixtures (png, jpg, pdf, docx, pptx),
  runtime configs (yaml), structured records (jsonl)
- `corpus/graph/` — multi-hop graph topologies
- `corpus/wikipedia/` — public-derived noise/OCR material

Do not add a corpus file without referencing it from a suite: unreferenced
files are dead weight and get removed.

## Case discipline (probe contract)

- Every `answerRequiredAll` / `answerRequiredAny` string must exist
  verbatim in the fixture that `expectedDocumentsContains` points to.
- `searchRequiredAll` must hold for the expected top document — prefer a
  token unique to that document.
- Numeric facts with units belong in `answerRequiredRegexAny` with
  patterns tolerant to inline markdown (`[*_\`]*` between number and
  unit); regexes run case-insensitively over the raw answer text.
- `allowedVerificationStates` may only contain successful states
  (`verified`, `partially_supported`); stricter (verified-only) is fine.
- No natural-language keyword dictionaries; anchors are identifiers,
  literals, and document-local structure.

### Per-item answer completeness

Cases may declare `expectedFacts` and `mandatoryCaveats` arrays. Each item has
these required fields:

| Field | Contract |
|---|---|
| `id` | Nonempty stable ID, unique across both arrays within the case |
| `fixture` | Path under `corpus/`, relative to this directory, listed in the suite's `documents` |
| `sourceExcerpt` | Nonempty verbatim UTF-8 excerpt from that fixture, including its original whitespace |
| `sourceSha256` | SHA-256 of the complete fixture bytes |
| `requiredAll` | Array of literal matchers, all required |
| `requiredAny` | Array of literal alternatives, at least one required when nonempty |
| `requiredRegexAny` | Array of regex alternatives, at least one required when nonempty |

At least one matcher array must be nonempty. Empty literal matchers, invalid
regexes, regexes matching empty text, missing sources, stale source digests,
non-verbatim excerpts, out-of-suite sources, absolute paths, traversal and
symlink escapes are rejected before running a case. Binary fixtures can remain
unannotated; this contract does not extract text from them.

The runner, native-surface replay and comparator use the same item evaluator.
Before matching, it decodes HTML entities, unwraps paired inline Markdown
emphasis and code, and replaces simple inline links with their display text.
It casefolds text and normalizes whitespace and Unicode dashes, preserving
line boundaries for regex anchors and underscores inside identifiers. Regex
matching is case-insensitive over this text. All nonempty matcher groups must
pass for an item to match. Excerpts establish fixture provenance; matchers are
separate so an answer need not repeat the excerpt's source formatting.

`answerCompleteness.policy` identifies the evaluator with version
`source-backed-inline-markdown-v2` and a SHA-256 over that version and the exact
fact/caveat annotations. Change the version when normalization semantics change.
Declensions, phrase order and interval references belong in source-backed item
alternatives, not language or product routing in the evaluator. Strict retrieval,
verification, preceding-turn probes and their aggregation are unchanged.
An offline rescore is a new policy observation: retain its policy identity and
original capture hashes separately, without overwriting frozen scores or treating
it as a new product run. Report item gains separately from answer and strict passes.

`answerCompleteness` reports `matchedIds`, `missingIds`, `matchedCount`,
`totalCount` and `fraction` separately for facts and caveats. A missing fact or
mandatory caveat adds `answer_facts` or `answer_caveats` to `failedChecks` and
fails `answerPass` and `strictCasePass`. Existing required, forbidden, reference
and verification checks still apply. This is deterministic literal/regex
coverage, not a semantic correctness or citation-entailment score.

Cases without items report `status: not_annotated`, zero item counts and null
fractions, never 100%. Failed executions report `unavailable`. Suite and matrix
summaries report annotated, not-annotated and unavailable case counts,
`annotationCoverage`, and item-weighted fractions over available annotated
cases. Missing historical observations are unavailable to the runner's summary.

The comparator recomputes items from raw answer evidence, not stored scores.
Its `answerCompleteness` section includes both summaries and paired item
regressions. A pair is comparable only when both cases are annotated with
identical item definitions and provenance. Historical unannotated pairs have
`comparable: false` and null regressions, without a new default gate. Existing
correctness checks enforce annotated requirements; existing integrity checks
still reject changed definitions. Item metadata is included in
`caseDefinitionSha256` and `suiteDefinitionSha256`; `corpusSha256` remains a
digest of source bytes only. Empty annotation arrays do not change the
definition identity of an unannotated case.

### Native multi-turn cases

A case may add an ordered `precedingTurns` array. Each entry has a nonempty
stable `id` (unique within the conversation, including the final case ID), a
nonempty `question`, and answer expectations. Supported expectations are
`answerRequiredAll`, `answerRequiredAny`, `answerRequiredRegexAny`,
`answerOpeningRequiredAny`, `answerForbiddenAny`, `answerForbiddenRegexAny`,
`allowedVerificationStates`, `expectedFacts` and `mandatoryCaveats`.
Literal/regex fields are arrays of nonempty strings; regexes must compile and
must not match empty text. At least one answer matcher or fact/caveat is required.
Verification defaults to `verified`; an explicit list may also allow
`partially_supported`, never failed states. Facts/caveats use the source-backed
schema above and the same evaluator as the final answer.

The runner posts these questions, then the final case question, to the same
native `/query/sessions/{id}/turns` endpoint. It creates no client-side transcript
or provider retry. Cases still receive separate sessions. Only the final turn
runs the auxiliary retrieval/rank probe; preceding turns do not inherit final
reference-count or retrieval expectations. Every turn uses canonical response
validation and `/query/executions/{id}` detail checks. Multi-turn histories also
require the selected session and distinct execution IDs.

`turnExecutions` preserves ordered questions, execution IDs, answers, raw API
evidence, failures, answer latency and cost observations, including the final
turn. A failed expectation does not prevent subsequent turns, but fails the
conversation even if the final answer passes. An execution/identity error stops
the conversation without retry and retains completed turns and any received
partial response. `conversationHistoryComplete` distinguishes this from a
completed history. The comparator recomputes each turn from raw evidence and
requires matching turn policies; missing historical turn evidence is not
comparable. Turn definitions enter the case/suite definition hashes, not the
corpus byte hash.

`executionCost` covers the **final turn's root and typed descendant query executions**
(`final_turn_query_execution_tree`). `conversationCost` covers their union across
actual turns (`conversation_query_execution_union`), including each execution once, with
separate totals by currency and explicit complete/partial/unknown/unavailable
coverage counts. Incomplete or missing history never reports complete cost.
Suite/matrix summaries and comparator reports expose both scopes. The opt-in
cost regression gate checks final-turn tree cost and, for multi-turn inputs,
conversation cost separately; changed turn definitions or missing histories
cannot satisfy that gate. Historical single-turn query cost is not substituted
for conversation cost.

`answerLatencyMs` still times only the final turn POST. `conversationLatencyMs`
is elapsed wall time from the first turn through the final evaluation, including
execution-detail/billing reads and the final rank probe; it is not an answer
latency percentile or a new latency gate.

The existing `inventory_wsdl` technical-contract case now switches from a
checkout REST endpoint question to the inventory SOAP WSDL question. Both are
backed by the suite's existing source fixtures; the registry case count is
unchanged. Offline mocks exercise this runner contract. No live multi-turn
answer-quality or latency result is claimed by this source change.

## Running

Explicitly select the authorized live target and its independently trusted TLS
certificate fingerprint. Neither entrypoint chooses a deployment by default;
plain HTTP and unpinned TLS are not supported:

```bash
export IRONRAG_BENCHMARK_BASE_URL="https://<authorized-host>/v1"
export IRONRAG_BENCHMARK_TLS_CERTIFICATE_SHA256="<trusted-lowercase-sha256>"
export IRONRAG_SESSION_COOKIE="..."
export IRONRAG_BENCHMARK_WORKSPACE_ID="..."
```

Seed a fresh library with the full registry corpus, then run it:

```bash
make benchmark-golden-seed
make benchmark-golden IRONRAG_BENCHMARK_LIBRARY_ID=<library-id>
```

Per-dimension convenience targets: `benchmark-grounded-noisy-layout`,
`benchmark-grounded-multihop`, `benchmark-grounded-technical`.

Each run writes `matrix.result.json`, one `*_suite.result.json` per
suite, and appends rank metrics (`hit@k`, `MRR`) to
`rank_metrics_trend.jsonl` in the output directory. Compare two runs:

```bash
python3 apps/api/benchmarks/grounded_query/compare_benchmarks.py \
  <baseline-dir> <candidate-dir>
```

### Query execution cost

After the synchronous turn returns its completed execution, the runner reads
`GET /v1/billing/executions/query_execution/{executionId}` and
`GET /v1/query/executions/{executionId}/llm-context`. Native billing owns exactly
one execution, not its subtree. The runner follows only the snapshot's typed
`iterations[].childQueryExecutionIds`, fetching each distinct child's execution
detail, snapshot and native billing. It never infers parent database edges or
extracts IDs from prompts, response text or tool-result JSON. Ingest, auxiliary
search probes and host cost are excluded. Native prices and currencies are
retained without currency conversion or repricing.

Each case has one `executionCost` observation:

- `status: measured`, `evidence: {rootExecutionId, components}`, `error: null`
- `status: unavailable`, `evidence: <partial evidence or null>`,
  `error: <exception type>` when collection or attribution fails. Native evidence
  already collected is retained, but is not included in measured totals. The
  answer and quality results survive; exception messages are not persisted.
- `status: not_measured`, `evidence: null` when case execution itself failed

Each component retains the unmodified native `billing`, `executionDetail` and
`llmContext` objects. There is no synthetic native billing DTO or trusted saved
aggregate. The comparator revalidates closure, scope, identity, timing and native
call counts, then recomputes coverage and unique totals from components.
Billing retains `totalCost` as a decimal string, `currencyCode`, provider
and unpriced call counts, `costCoverage`, execution identity, and API timestamp.
`complete` means all captured calls were priced. `partial` is an observed
lower bound; `unknown` means all calls are unpriced. The API explicitly returns
complete zero USD for an execution with no provider calls. That does not establish
zero subtree cost: all descendants and their snapshots must also be available.
Missing, malformed or incomplete typed provenance is unavailable, never complete zero.

Collection is bounded to 64 distinct executions, depth 8, and 256 typed references
and iterations. Cycles, excessive fanout/depth, cross-workspace/library children,
unreferenced or missing components, duplicate roots and conflicting shared
components are refused. Repeated typed child references are charged once, including
across turns/cases. Child lifetimes must fit within each referring parent's
execution window. Historical cached children are therefore not billed again.
In-loop tool replay emits no new typed child IDs; cache replay responses whose
turn identities differ from the original execution cannot establish incurred
cost and are unavailable. Unsupported replay attribution has no fallback.

Suite and matrix `summary.executionCost` contain `coverageCounts` (including
`unavailable` and `not_measured`), `observedTotalsByCurrency` as exact decimal
strings, `invalidCaseIds`, and `complete`. Totals include measured partial and
unknown observations; they are not full spend unless coverage is complete.
Currencies are never added together. Older result files without observations
are reported as `not_measured`; old single-native-DTO observations are unsupported
and unavailable, since they cannot establish lineage completeness. Existing
artifacts are not rewritten. Summary-only totals are not invented or trusted.

The comparator prints and writes coverage changes, per-currency observed deltas,
and paired `coverageDowngrades`. Cost does not affect exit status by default.
Opt in with a nonnegative decimal percentage:

```bash
python3 apps/api/benchmarks/grounded_query/compare_benchmarks.py \
  <baseline-dir> <candidate-dir> --max-cost-regression-percent 10
```

The cost gate requires nonempty, complete observations for the same suite/case
keys and the same currency set per pair. Missing historical cost, invalid evidence,
partial coverage, or changed currencies fail this opt-in gate rather than pass
as zero. Each currency's total must stay within the percentage budget. A zero
baseline permits only zero candidate cost; a positive increase has no finite
percentage (`regressionPercent: null`). Other quality, integrity, and latency
gates retain their existing behavior.

Certification protocol: paired runs on a quiet host (the runner enforces
PSI/swap-IO eligibility), deterministic bindings (temperature 0.0,
top_p 1.0), compare failure SETS across the pair — totals alone prove
nothing.
