import { describe, expect, it } from 'vitest'

import { formatCostWithCoverage } from './costCoverage'

const formatAmount = (amount: number) => `$${amount.toFixed(3)}`
const partialLabel = (count: number) => `+${count} unpriced`

describe('formatCostWithCoverage', () => {
  it('does not render an amount when coverage is unknown', () => {
    expect(
      formatCostWithCoverage({
        amount: 0,
        coverage: 'unknown',
        formatAmount,
        partialLabel,
        unknownLabel: 'Cost unknown',
        unpricedProviderCallCount: 2,
      }),
    ).toBe('Cost unknown')
  })

  it('marks a partial amount with its unpriced provider-call count', () => {
    expect(
      formatCostWithCoverage({
        amount: '1.25',
        coverage: 'partial',
        formatAmount,
        partialLabel,
        unknownLabel: 'Cost unknown',
        unpricedProviderCallCount: 3,
      }),
    ).toBe('$1.250 (+3 unpriced)')
  })

  it('can hide only a fully covered zero amount', () => {
    expect(
      formatCostWithCoverage({
        amount: 0,
        coverage: 'complete',
        formatAmount,
        hideCompleteZero: true,
        partialLabel,
        unknownLabel: 'Cost unknown',
        unpricedProviderCallCount: 0,
      }),
    ).toBeNull()
    expect(
      formatCostWithCoverage({
        amount: 0,
        coverage: 'partial',
        formatAmount,
        hideCompleteZero: true,
        partialLabel,
        unknownLabel: 'Cost unknown',
        unpricedProviderCallCount: 1,
      }),
    ).toBe('$0.000 (+1 unpriced)')
  })
})
