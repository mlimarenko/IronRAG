import { describe, expect, it } from 'vitest'
import type { TFunction } from 'i18next'

import i18n from '@/shared/i18n'
import { gridFacetKey, gridFacetValue } from '@/shared/data-grid'
import {
  costFacetOption,
  librariesColumnFilters,
  libraryFacetLabel,
  libraryFacetOptions,
  librarySelectedFacets,
} from './librariesGrid'
import { costValueKey, decodeCostValueKey } from './libraryCost'

const t = i18n.t.bind(i18n) as unknown as TFunction

const usd = {
  amount: '1.23',
  currencyCode: 'USD',
  coverage: 'complete' as const,
  unpricedCallCount: 0,
}
const usdPartial = {
  amount: '1.23',
  currencyCode: 'USD',
  coverage: 'partial' as const,
  unpricedCallCount: 2,
}
const eur = {
  amount: '1.23',
  currencyCode: 'EUR',
  coverage: 'complete' as const,
  unpricedCallCount: 0,
}

describe('libraries grid column filters', () => {
  it('cost option keys survive the canonical grid fallback decoder and preserve a real NULL bucket', () => {
    expect(gridFacetValue(costFacetOption(usdPartial, 1, t, 'en').key)).toBe(
      costValueKey(usdPartial),
    )
    expect(gridFacetValue(costFacetOption(null, 1, t, 'en').key)).toBeNull()
  })
  it('does not reinterpret an invalid selected cost key as the NULL bucket', () => {
    expect(() => librariesColumnFilters({ cost: [gridFacetKey('malformed')] })).toThrow()
    expect(librariesColumnFilters({ cost: [gridFacetKey(null)] })?.cost).toEqual([null])
  })
  it('converts typed facet keys into typed filter values, keeping the null bucket', () => {
    const filters = librariesColumnFilters({
      library: [gridFacetKey('Manuals')],
      documents: [gridFacetKey(12), gridFacetKey(null)],
      readiness: [gridFacetKey(true)],
      cost: [gridFacetKey(costValueKey(usd))],
      queue: [],
    })
    expect(filters).toEqual({
      library: ['Manuals'],
      documents: [12, null],
      readiness: [true],
      cost: [usd],
    })
  })

  it('returns undefined when nothing is selected', () => {
    expect(librariesColumnFilters({ library: [] })).toBeUndefined()
  })

  it('keeps amount, coverage and unpriced count distinct in compound cost keys', () => {
    const keys = new Set([costValueKey(usd), costValueKey(usdPartial), costValueKey(eur)])
    expect(keys.size).toBe(3)
    expect(decodeCostValueKey(costValueKey(usdPartial))).toEqual(usdPartial)
    expect(decodeCostValueKey('company:1.23')).toBeNull()
  })

  it('preserves cost metadata through the column filter', () => {
    const filters = librariesColumnFilters({
      cost: [
        gridFacetKey(costValueKey(usd)),
        gridFacetKey(costValueKey(eur)),
        gridFacetKey(costValueKey(usdPartial)),
      ],
    })
    expect(filters?.cost).toEqual([usd, eur, usdPartial])
  })

  it('labels selected facets the same way the menu offered them', () => {
    const selected = librarySelectedFacets({ readiness: [gridFacetKey(true)] }, t, 'en')
    expect(selected.readiness?.[0]?.label).toBe('Ready')
    expect(selected.readiness?.[0]?.count).toBe(-1)
  })

  it('labels the cost facet with the same formatter the cell uses', () => {
    const option = costFacetOption(usdPartial, 3, t, 'en')
    expect(option.label).toContain('1.23')
    expect(option.label).toContain('2')
    expect(option.key).toBe(gridFacetKey(costValueKey(usdPartial)))
    expect(costFacetOption(null, 1, t, 'en').label).toBe('Cost unknown')
  })

  it('maps typed cost facet rows and keeps the NULL bucket', () => {
    const options = libraryFacetOptions(
      'cost',
      [
        {
          count: 1,
          decimal: '1.23',
          currencyCode: 'USD',
          coverage: 'complete',
          unpricedCallCount: 0,
        },
        { count: 1, decimal: null, currencyCode: null, coverage: null, unpricedCallCount: null },
      ],
      t,
      'en',
    )
    expect(options[0]?.key).toBe(gridFacetKey(costValueKey(usd)))
    expect(options[1]?.label).toBe('Cost unknown')
  })

  it('formats numeric and lifecycle facet labels from the displayed value', () => {
    expect(libraryFacetLabel('queue', 1234, t, 'en')).toBe('1,234')
    expect(libraryFacetLabel('lifecycle', 'archived', t, 'en')).toBe('Archived')
  })
})
