# Billing and cost reporting

How a provider call becomes a reported cost, and what a reported number means.

## 1. The four stages

| Stage | Table | Written when |
|---|---|---|
| Reservation | `billing_provider_call` | before a paid request is issued (state `started`) |
| Usage | `billing_usage` | after the provider responds, one row per billing unit (input / output / cached input tokens) |
| Charge | `billing_charge` | after usage is recorded; reads the applicable rate from the model row and always writes |
| Rollup | `billing_execution_cost` | recomputed per execution after usage changes |

The reservation exists before the request so an ambiguous timeout can always
be reconciled. Usage is committed before any terminal query outcome. For new
calls, model validation guarantees the pricing needed to write charges.

## 2. Models carry pricing

Pricing is part of each `ai_model_catalog` row. It contains `currencyCode`
and per-one-million-token rates for input, cached input, cache-write input,
and output. A model may also define a long-context tier with
`thresholdTokens` and input, cached-input, and output rates. Model create and
update requests require pricing, so a missing applicable price is
structurally impossible for new calls.

Every seeded model carries real vendor rates. Startup refreshes seeded model
rows from the seed, so operator price edits to those rows are overwritten on
restart. Models created by an operator retain their configured values.

**Prices apply at capture time.** `billing_charge` copies `unit_price` from
the model pricing in effect when usage is captured and does not reference a
separate pricing row. Changing a model price never rewrites charge history.
Historical repricing would require an explicit backfill that dirties the
affected rollups; there is none today.

## 3. What a reported cost means

Every cost read (`BillingExecutionCost`, `DocumentCostSummary`,
`LibraryCostSummary`, `WorkspaceCostSummary`) carries three numbers that
must be read together:

- `totalCost` — sum of charges actually written.
- `providerCallCount` — all calls in scope, counted from
  `billing_provider_call`, priced or not.
- `unpricedProviderCallCount` — historical calls that recorded usage with at
  least one unit that had no price. A partially priced call counts once, so
  this number is comparable with `providerCallCount`.

`costCoverage` (`CostCoverageStatus`) derives from the last two and is the
field a client should branch on. New calls are always `complete`; stored
calls with missing prices use `partial` or `unknown`:

| Coverage | Meaning | What a UI must show |
|---|---|---|
| `complete` | every call priced (or no calls at all) | the total |
| `partial` | some calls priced, some not | the total, marked as a lower bound with the unpriced count |
| `unknown` | calls exist, none priced | "cost unknown", never `$0.00` |

A zero total alone is ambiguous and must never be rendered as "free".

## 4. Rollup consistency rules

- The unpriced count is computed inside the same transaction and advisory
  lock as the charge aggregate, then written with the rest of the rollup, so
  a reader never mixes counts from two generations.
- Document lists and facets read rollup health, rows and counts in one
  read-only repeatable-read snapshot, even when there are no matching rows.
  Their required `scopePending` field describes query availability separately
  from `costRollupFreshness`. A metadata-only scope stays available while its
  cost cells are withheld. Cost sorting, effective cost filters and global
  text search require current costs; while reconciling they return a pending
  scope with no rows, exact counts or continuation. Facets exclude their own
  column filter before deciding cost dependence. A cost facet always depends
  on current costs. The client restarts a pending cost cursor chain on the
  first page without resetting the pinned clock.
- The billing cost-summary and document-costs reads do not 503 on a dirty
  generation. They answer with an explicit, coverage-independent
  `costRollupFreshness`: `current` means every committed provider call is
  folded into the rollup; `reconciling` means at least one owning execution
  lags. A `reconciling` summary withholds every value field (`null`, never a
  fake zero or a stale total), and a reconciling document-cost page returns an
  empty list with no cursor and no count. A workspace aggregate is
  `reconciling` when any of its libraries is; a clean library stays `current`.
  A terminal mixed-currency rollup is still a 409.
- Unpriced usage is not a currency conflict. The mixed-currency guard stays
  about disagreement among actual charge currencies; a scope with no charges
  reports the placeholder `USD` and its coverage status dominates rendering.
- An execution with no rollup row captured no billable call at all, so its
  zero total is `complete`. Executions whose calls exist but have no price
  do have a rollup row, and that row reports the unpriced calls.

## 5. Stored unpriced calls

Stored unpriced calls retain their coverage and rollup handling during migrations
and maintenance. Report partially priced usage as a lower bound and wholly
unpriced usage as unknown cost, never as free usage. New charge paths
must treat an absent model rate as an invariant violation, not as a reason to
skip a charge.
