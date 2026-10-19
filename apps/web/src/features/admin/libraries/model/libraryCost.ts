import type { AdminLibraryCostSummary, AdminLibraryCostValue } from '@/shared/api/generated'
import type { CostSummary } from '@/shared/format'

/**
 * The canonical cost projection the server emits for one library row and one
 * cost facet value — the single shape the cell, the facet label, the column
 * filter and the sort all read. `amount` is the rounded display decimal (two
 * fraction digits) carried as source text so no float ever touches it, and
 * `currencyCode` scopes it so an amount never matches another currency's row.
 */

/**
 * The canonical grid key of one cost value. The shared grid stores keys as
 * strings, so a compound value needs a stable textual identity; JSON of the
 * ordered fields is that identity, decoded and validated at the boundary.
 */
export function costValueKey(value: AdminLibraryCostValue): string {
  return JSON.stringify({
    amount: value.amount,
    currencyCode: value.currencyCode,
    coverage: value.coverage,
    unpricedCallCount: value.unpricedCallCount,
  })
}

function isCoverage(value: unknown): value is AdminLibraryCostValue['coverage'] {
  return value === 'complete' || value === 'partial' || value === 'unknown'
}

/**
 * Decode a grid cost key back to its canonical value, validating every field.
 * A key that is not a well-formed cost object is not a cost selection, so a
 * caller cannot smuggle a partial or mismatched value into a request.
 */
export function decodeCostValueKey(key: string): AdminLibraryCostValue | null {
  let parsed: unknown
  try {
    parsed = JSON.parse(key)
  } catch {
    return null
  }
  if (typeof parsed !== 'object' || parsed === null) return null
  const candidate = parsed as Record<string, unknown>
  if (
    typeof candidate.amount !== 'string' ||
    !/^\d+(\.\d{1,2})?$/.test(candidate.amount) ||
    typeof candidate.currencyCode !== 'string' ||
    !/^[A-Z]{3}$/.test(candidate.currencyCode) ||
    !isCoverage(candidate.coverage) ||
    typeof candidate.unpricedCallCount !== 'number' ||
    !Number.isSafeInteger(candidate.unpricedCallCount) ||
    candidate.unpricedCallCount < 0
  ) {
    return null
  }
  return {
    amount: candidate.amount,
    currencyCode: candidate.currencyCode,
    coverage: candidate.coverage,
    unpricedCallCount: candidate.unpricedCallCount,
  }
}

/**
 * The canonical cost value of one row: the rounded projection when the server
 * reported a readable rollup, and null — the real NULL bucket — when the rollup
 * is blocked or mixes currencies. The lossless `costSummary` stays untouched for
 * the metrics strip.
 */
export function rowCostValue(row: {
  costValue?: AdminLibraryCostValue | null
  costSummary: AdminLibraryCostSummary | null
}): AdminLibraryCostValue | null {
  if (row.costValue !== undefined && row.costValue !== null) return row.costValue
  return null
}

/** Adapt a row's canonical cost value into the shared `formatCost` summary shape. */
export function costValueToSummary(value: AdminLibraryCostValue | null): CostSummary | null {
  if (!value) return null
  return {
    // The admin grid emits only already-resolved, readable cost values; the
    // freshness marker exists so the shared formatter can withhold a stale
    // amount, and here there is none to withhold.
    costRollupFreshness: 'current',
    totalCost: value.amount,
    currencyCode: value.currencyCode,
    providerCallCount: 0,
    unpricedProviderCallCount: value.unpricedCallCount,
    costCoverage: value.coverage,
  }
}
