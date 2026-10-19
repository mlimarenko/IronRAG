import type { CostCoverageStatus } from '@/shared/api/generated'

type CostAmount = string | number | null | undefined

type FormatCostWithCoverageOptions = Readonly<{
  amount: CostAmount
  coverage: CostCoverageStatus | null | undefined
  formatAmount: (amount: number) => string
  hideCompleteZero?: boolean
  partialLabel: (unpricedProviderCallCount: number) => string
  unknownLabel: string
  unpricedProviderCallCount: number
}>

function parseCostAmount(value: CostAmount): number | null {
  if (value == null || value === '') return null
  const amount = Number(value)
  return Number.isFinite(amount) ? amount : null
}

/** Formats an amount only when the API states how complete that amount is. */
export function formatCostWithCoverage({
  amount: rawAmount,
  coverage,
  formatAmount,
  hideCompleteZero = false,
  partialLabel,
  unknownLabel,
  unpricedProviderCallCount,
}: FormatCostWithCoverageOptions): string | null {
  if (coverage === 'unknown') return unknownLabel
  if (coverage == null) return null

  const amount = parseCostAmount(rawAmount)
  if (amount == null) return null
  if (hideCompleteZero && coverage === 'complete' && amount <= 0) return null

  const formatted = formatAmount(amount)
  return coverage === 'partial'
    ? `${formatted} (${partialLabel(unpricedProviderCallCount)})`
    : formatted
}
