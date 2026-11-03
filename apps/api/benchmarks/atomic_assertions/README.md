# Atomic assertion pilot

This experiment compares generated propositions with the existing canonical
structured blocks, typed technical facts and source-backed context. It does not
replace sources, introduce a second knowledge base or change production search.

## Native baseline

`make backend-bench-atomic-assertion-input` consumes `ASSERTION_CANONICAL_INPUT`
and `ASSERTION_CANONICAL_INPUT_SHA256`, calls the existing `TechnicalFactService`,
and creates `ASSERTION_NATIVE_EXPORT`. `ASSERTION_SOURCE_SHA` identifies the
executed source. The output distinguishes the comparison source from the source
that prepared the immutable canonical blocks. Facts, qualifiers, support IDs,
conflicts, extraction time and logical serialized bytes are retained.

The shared chunking replay accepts optional `technicalFacts` on each case and
`ASSERTION_RETRIEVAL_LANE=chunks|structured_blocks|technical_facts`. It uses actual
`PgSearchStore` methods. The default remains the chunk lane. All original pinned
input, explicit loopback-only fresh-database, current-revision publication and
create-new journal guards remain in force. See the neighboring chunking contract
for the `CHUNKING_*` inputs and exact-arm selector.

The structured-block lane admits only selected arms whose block collection is
identical to the canonical source blocks. Derived split-block treatments are
refused before database access; their hydration is outside this pilot contract.

Technical-fact inputs require canonical block supports. Opaque original chunk
links are refused because this export has no original chunk-ID registry. Fresh
chunk links are derived from retained block supports. The seed keeps original
support blocks unchanged and remaps all identities into the new database scope.

Structured/fact observations hydrate persisted source blocks and, for facts,
qualifiers. They report at most 16 whole support blocks per hit, with explicit
truncation. A payload over 1 MiB per hit is refused, not clipped. Native search
latency excludes this hydration; it is not assembled-answer latency. Source
availability is a separate measurement from the correctness of a generated
assertion. The production authoritative context also restores supporting blocks
and renders fact qualifiers, so a value-only hit is not a fair complete baseline.

## First proposition extraction

Freeze literal source-unit inputs and a separate factual binding oracle before
generation. Retrieval-oracle answer spans may lack an independent subject,
property, scope or continuation: include the necessary actual source owners and
headings without inventing labels, headers or repaired values. Disclose any
gold-source curation rather than calling it end-to-end extraction recall.

The neighboring offline `chunking/generate_answers.py` runner can consume these
source-only units through its existing input contract. Each row receives the same
extraction instruction, with no evaluation question or expected statement. Use a
new network-denied resource-bounded process and create-new output directory.
Reusing pinned model/package bytes does not restart a former runtime or admit a
hosted provider call.

`propositions.py` consumes the pinned first generation report and unit oracle:

```sh
python propositions.py \
  --report first/report.json --report-sha256 REPORT_SHA256 \
  --units unit-oracle.json --units-sha256 ORACLE_SHA256 \
  --output first-candidates.json
```

It accepts only `{"claims":[...]}` with string fields `subject`, `assertion`,
`negation`, `conditions`, `time`, `units` and `scope`. Empty claims, invalid JSON,
schema failures and output-cap termination remain distinct observations. It does
not strip fences, repair text, retry or infer missing claims. Source linkage
comes from the frozen unit transport; it is not a semantic truth judgment.

Assess every frozen fact for subject, negation, qualifiers, time reference, units,
ownership, omissions, unsupported additions and atomicity. A standalone assertion
must preserve its inseparable qualifications even if auxiliary fields also
contain them. Preserve malformed or empty outputs in the population. If the
admitted assertion population cannot support the frozen corpus, report that
representation failure instead of padding it with original chunks or synthetic
claims and calling it a proposition retrieval comparison.

Measure actual input/output tokens, generation time, memory, logical volume,
native retrieval and physical storage separately. A small local model and a
curated corpus support a bounded adoption/non-adoption decision, not a general
claim about proposition representations or production answer behavior.
