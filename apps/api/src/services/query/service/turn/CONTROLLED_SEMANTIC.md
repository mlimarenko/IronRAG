# Authored semantic candidate measurement

`controlled_semantic::measure_authored_candidates` is the single opt-in library
test for measuring frozen authored answers. It is not a public endpoint or an
acceptance oracle. Ordinary tests ignore it. With either explicit input absent,
it refuses before settings, credentials, clients, database or provider setup.

No command below grants target access or spending. Before a real invocation,
the operator must obtain fresh independently accepted target, corpus and native
financial admission, including the selected route, complete wire/token/output
bounds, native physical lifetime allowance, immutable expiry and unknown-call
liability. The admission JSON records that external decision; it does not create
or authenticate a financial grant. Never reuse a retired key, caller or clock.

## Supported invocation

Build from the exact clean accepted source and record the resulting library test
binary SHA-256. Use the repository's native Cargo admission wrapper and a distinct
target directory. Build/test listing does not run the ignored measurement:

```sh
CARGO_TARGET_DIR=<worktree-target> cargo test --locked --lib --all-features \
  -p ironrag-backend --no-run
```

After separate operational and financial permission, use that exact test binary:

```sh
IRONRAG_SEMANTIC_CONTROLS_MANIFEST=<absolute-manifest.json> \
IRONRAG_SEMANTIC_CONTROLS_ADMISSION=<absolute-admission.json> \
<accepted-test-binary> --ignored --exact \
  services::query::service::turn::controlled_semantic::measure_authored_candidates
```

The explicit settings must include `IRONRAG_DATABASE_URL`, `IRONRAG_REDIS_URL`,
the existing native credential configuration and
`IRONRAG_PROVIDER_RETRY_MAX_ATTEMPTS=1`. Their values must not enter argv, logs or
receipts. No dependency, schema, source document, credential or binding is created
by setup. The admitted operation later writes audited reads, fresh controlled
conversation/query/runtime contexts, nonterminal provenance and provider billing.
It must run against the independently accepted isolated synthetic scope with one
writer. It does not overwrite generated finals or their original outcomes.

## Input contracts

All names below use JSON camelCase. Input files are bounded at 262144 bytes.
Paths are operator-supplied files, not inline credentials.

- Manifest: `protocol` = `ironrag-semantic-controls/1`, `protocolVersion` = 1,
  clean `sourceSha`, `caseFile`, `caseFileSha256`, `selectedCaseIds`, `workspaceId`,
  `libraryId`, `databaseUrlSha256`, `redisUrlSha256`, `sessionFile`, `sessionSha256`,
  `sessionCookieJsonPointer`, `preservationFile`, `preservationSha256`, `sources`.
- Each source pin: `label`, `sourceFile`, `sourceFileSha256`, `documentId`,
  `revisionId`, `canonicalTextSha256`, `canonicalUtf8Bytes`. Complete evidence is
  limited to the production verifier's 22000-byte evidence budget. This owner
  accepts Markdown only, so native full-read preparation can deny all provider
  operations rather than invoke image extraction or embeddings.
- Case file: existing `sources` map with `sha256` and optional
  `completeDocumentLines`; `cases` with `id`, `source`, `question`, `answer`.
  Other annotation fields, including expected labels, never reach the evaluator.
  Duplicate IDs, unknown selection, changed bytes and missing sources fail closed.
- Preservation: `lifecycleId`, `workspaceId`, `libraryId`,
  `independentReviewer`, `sources`. Each source records its `label`, `documentId`,
  `revisionId`, `sourceFileSha256`, `canonicalTextSha256`, `canonicalUtf8Bytes`,
  `completeSourceCoverageVerified`, `coverageEvidenceFile` and
  `coverageEvidenceSha256`. Hashes alone do not establish semantic preservation;
  the independent complete-coverage receipt and its evidence are required.
- Admission: `protocol` = `ironrag-semantic-control-admission/1`, `lifecycleId`,
  absolute canonical private `ledgerRoot`, `manifestSha256`, `runnerSha256`,
  `binarySha256`, `buildReceiptFile`, `buildReceiptSha256`,
  `independentReviewer`, `freshGrantReference`, `notBefore`,
  `hardStopAt`, `maxPhysicalCalls`, `bindingId`, `accountId`, `providerCatalogId`,
  `modelCatalogId`, `providerKind`, `modelName`, `gatewayBaseUrl`. `runnerSha256`
  hashes this version's `controlled_semantic.rs`; `binarySha256` hashes the actual
  executable. Selected case count must equal the admitted physical allowance.
  The selected binding must match all identities and use the existing native
  OpenAI-compatible strict JSON-schema transport. At least the full native
  12-second request budget must remain before expiry for every case.
- Build receipt: `protocol` = `ironrag-semantic-control-build/1`, `sourceSha`,
  `binarySha256`, `runnerSha256`, `independentReviewer`,
  `reproducibleSourceBindingVerified`, `evidenceFile`, `evidenceSha256`. The
  independent reviewer must actually verify the binary-to-clean-source binding
  and preserve reproducible build evidence before accepting this receipt. A
  checkout SHA and unrelated executable hash alone do not establish that link.

## Native execution and evidence

Each case authenticates the real pinned UI session and authorizes query access,
performs audited native full `read_document`, uses the canonical document-reference
parser and persists provenance through the existing owners. It verifies the
entire current canonical revision, raw-file checksum, complete native slice and
independent preservation evidence. The provider-denying preparation gateway is
never used for measurement. The production selected verifier then rechecks
evidence, applies its complete schema/response/policy and retains native billing
attribution. Expected labels do not decide the result.

The complete selected binding, including account, gateway, credential and wire
controls, is pinned in a test-only scope. The production provider validates its
actual resolved binding against that pin before request reservation or I/O;
changed bindings fail closed rather than dispatch and check attribution later.

There is at most one logical selected verifier invocation and at most one native
gateway attempt per case: no receipt reuse, semantic repair or transport retry.
Preparation is provider-denied; a denied preparation operation fails the case.
Serialized request <=96000 bytes, output <=4096 tokens/65536 bytes and the native
12-second timeout remain owned by the production verifier. These byte limits are
not token, monetary or final-wire bounds. All physical egress, actual wire shape,
pricing and permanent-stop enforcement still require separate native financial
qualification. UI/MCP generated finals are separate workflows, not included in
this controlled-case count or allowance.

The admitted database owns one native nonterminal lifecycle context whose ID is
the lifecycle UUID. Its immutable identity records the canonical `ledgerRoot`;
a native advisory transaction lock serializes first creation. Another root for
that lifecycle is rejected before any case dispatch, even if an admission file
is moved or replaced. Keep this context and its ledger for the complete lifecycle;
do not delete, relabel or manually repair either to permit replay. The ignored
offline native tests exercise PostgreSQL anchor creation, idempotency, scope and
root refusal, concurrent ownership, and resolved binding drift in disposable
databases with provider dispatch denied. They do not replace observation in an
admitted target or prove provider wire behavior or cost.
Exclusive synced `started` records precede case-native operations;
their identity binds the question, answer, protocol and source evidence, excluding
mutable annotation paths, subset order and newly allocated execution IDs.
Interrupted starts stay reserved. `prepared`, `observed` and failure records
retain first observations; no replay or cleanup is automatic. Refusal, timeout,
invalid protocol/provenance, not-run and unknown results remain unsuccessful or
unknown and stop dependent cases. A successful test means measurement completed,
not that every authored answer was supported: every observation explicitly says
`semanticAcceptance: not_evaluated`. Semantic accuracy, false refusals, latency,
cost and UI/MCP parity require the separate accepted comparison.
