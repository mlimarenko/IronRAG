import { describe, expect, it } from 'vitest'

import { formatRelative, formatCost, formatDocumentSize } from './format'

describe('formatDocumentSize', () => {
  it.each([null, undefined])('renders absent size %s without claiming zero bytes', (size) => {
    expect(formatDocumentSize(size)).toBe('—')
  })
  it('renders actual zero bytes separately from a missing publication size', () => {
    expect(formatDocumentSize(0)).toBe('0 B')
    expect(formatDocumentSize(1024)).toBe('1.0 KB')
  })
})

describe('formatCost decimal source', () => {
  it('does not change digits above the floating-point integer precision limit', () => {
    expect(
      formatCost(
        {
          costCoverage: 'complete',
          costRollupFreshness: 'current',
          totalCost: '9007199254740993.01',
          currencyCode: 'USD',
          providerCallCount: 1,
          unpricedProviderCallCount: 0,
        },
        'unknown',
        'unpriced',
        'en',
      ),
    ).toBe('USD 9,007,199,254,740,993.01')
  })

  it('replaces every Intl non-breaking space with an ASCII space, not just the first', () => {
    const formatted = formatCost(
      {
        costCoverage: 'complete',
        costRollupFreshness: 'current',
        totalCost: '1234567.5',
        currencyCode: 'USD',
        providerCallCount: 1,
        unpricedProviderCallCount: 0,
      },
      'unknown',
      'unpriced',
      'ru',
    )
    expect(formatted).not.toContain('\u00a0')
    expect(formatted).toBe('1 234 567,50 USD')
  })

  it('withholds a reconciling read instead of fabricating a zero amount', () => {
    expect(
      formatCost(
        {
          costCoverage: null,
          costRollupFreshness: 'reconciling',
          totalCost: null,
          currencyCode: null,
          providerCallCount: null,
          unpricedProviderCallCount: null,
        },
        'unknown',
        'unpriced',
        'en',
      ),
    ).toBe('unknown')
  })
})

describe('formatRelative', () => {
  /// The dashboard re-reads its aggregate every 2.5s and the API stamps the
  /// recompute time server-side, so a second-level label counted down, jumped
  /// back up, and flipped between "in 4 seconds" and "4 seconds ago" on the
  /// smallest clock difference between browser and server. It blinked on every
  /// poll and told the reader nothing.
  it('floors anything inside a minute to the just-now label, in both directions', () => {
    const now = Date.now()
    for (const offsetMs of [0, 5_000, 59_000, -5_000, -59_000]) {
      expect(formatRelative(new Date(now + offsetMs).toISOString(), 'en', 'just now')).toBe(
        'just now',
      )
    }
  })

  it('keeps counting once the gap reaches a minute', () => {
    const now = Date.now()
    expect(formatRelative(new Date(now - 61_000).toISOString(), 'en', 'just now')).toBe(
      '1 minute ago',
    )
    expect(formatRelative(new Date(now - 7_200_000).toISOString(), 'en', 'just now')).toBe(
      '2 hours ago',
    )
  })
})
