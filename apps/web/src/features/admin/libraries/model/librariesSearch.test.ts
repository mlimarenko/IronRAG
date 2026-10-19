import { beforeAll, describe, expect, it } from 'vitest'
import type { TFunction } from 'i18next'

import i18n from '@/shared/i18n'
import { formatCost } from '@/shared/format'
import {
  deriveLibraryFacetSearch,
  deriveLibrarySearch,
  normalizeLibrarySearchNumber,
} from './librariesSearch'

const t = i18n.t.bind(i18n) as unknown as TFunction

describe('shared grid NULL label search', () => {
  it.each(['cost', 'documents', 'calls'] as const)(
    'round-trips the rendered NULL token in %s',
    (column) => {
      expect(deriveLibraryFacetSearch(column, t('dataGrid.nullValue'), 'en', t)).toEqual({
        unavailable: true,
      })
    },
  )
  it('round-trips the rendered NULL token globally', () => {
    expect(deriveLibrarySearch(t('dataGrid.nullValue'), 'en', t).hints).toEqual({
      unavailable: true,
    })
  })
  it.each(['queue', 'library', 'workspace', 'readiness', 'lifecycle'] as const)(
    'does not widen %s',
    (column) => {
      expect(deriveLibraryFacetSearch(column, t('dataGrid.nullValue'), 'en', t)).toBeNull()
    },
  )
})

/** The partial label the shared formatter actually prints for one cost value. */
function partialLabel(
  amount: string,
  currencyCode: string,
  unpricedCallCount: number,
  locale: 'en' | 'ru',
): string {
  return formatCost(
    {
      costRollupFreshness: 'current',
      totalCost: amount,
      currencyCode,
      providerCallCount: 6,
      unpricedProviderCallCount: unpricedCallCount,
      costCoverage: 'partial',
    },
    i18n.t('common.costUnknown'),
    i18n.t('documents.unpriced'),
    locale,
  )
}

/** The complete label the shared formatter prints for one cost value. */
function completeLabel(amount: string, currencyCode: string, locale: 'en' | 'ru'): string {
  return formatCost(
    {
      costRollupFreshness: 'current',
      totalCost: amount,
      currencyCode,
      providerCallCount: 6,
      unpricedProviderCallCount: 0,
      costCoverage: 'complete',
    },
    i18n.t('common.costUnknown'),
    i18n.t('documents.unpriced'),
    locale,
  )
}

describe('library search derivation', () => {
  beforeAll(async () => {
    await i18n.changeLanguage('en')
  })

  it('sends a number term as a typed hint over the numeric columns', () => {
    const derivation = deriveLibrarySearch('12', 'en', t)
    expect(derivation.search).toBe('12')
    // A whole-table term names counts and an amount, without choosing a currency.
    expect(derivation.hints).toEqual({ numbers: [12], costs: [{ amount: '12.00' }] })
  })

  it('collapses an amount-only term to the canonical rounded display amount', () => {
    const derivation = deriveLibrarySearch('1.2345', 'en', t)
    // No currency in the term: the amount matches every coverage.
    expect(derivation.hints).toEqual({ costs: [{ amount: '1.23' }] })
  })

  it('carries currency and complete coverage from a full complete label', () => {
    const label = completeLabel('1234.56', 'USD', 'en')
    expect(deriveLibrarySearch(label, 'en', t).hints).toEqual({
      costs: [{ amount: '1234.56', currencyCode: 'USD', coverage: 'complete' }],
    })
  })

  it('round-trips the actual RU complete label with its display code', () => {
    const label = completeLabel('1234.56', 'USD', 'ru')
    expect(deriveLibrarySearch(label, 'ru', t).hints).toEqual({
      costs: [{ amount: '1234.56', currencyCode: 'USD', coverage: 'complete' }],
    })
  })

  it('round-trips a non-USD CAD complete label with its display code', () => {
    const label = completeLabel('1234.56', 'CAD', 'en')
    expect(deriveLibrarySearch(label, 'en', t).hints).toEqual({
      costs: [{ amount: '1234.56', currencyCode: 'CAD', coverage: 'complete' }],
    })
  })

  it('derives a partial cost constraint from the full partial label', () => {
    const label = partialLabel('1234.56', 'USD', 3, 'en')
    expect(label).toContain('\u2265')
    expect(deriveLibrarySearch(label, 'en', t).hints).toEqual({
      costs: [
        { amount: '1234.56', currencyCode: 'USD', coverage: 'partial', unpricedCallCount: 3 },
      ],
    })
  })

  it('derives a partial cost constraint from an RU partial label with grouping', () => {
    const label = partialLabel('1234.56', 'EUR', 7, 'ru')
    expect(deriveLibrarySearch(label, 'ru', t).hints).toEqual({
      costs: [
        { amount: '1234.56', currencyCode: 'EUR', coverage: 'partial', unpricedCallCount: 7 },
      ],
    })
  })

  it('constrains a currency-only term without inventing an amount', () => {
    expect(deriveLibrarySearch('EUR', 'en', t).hints).toEqual({
      costs: [{ currencyCode: 'EUR' }],
    })
  })

  it('keeps a standalone decimal amount as an all-coverage cost', () => {
    expect(deriveLibrarySearch('0.50', 'en', t).hints).toEqual({
      costs: [{ amount: '0.50' }],
    })
  })

  it('accepts a leading marker with an amount and a bare marker', () => {
    expect(deriveLibrarySearch('\u2265 1.23', 'en', t).hints).toEqual({
      costs: [{ amount: '1.23', coverage: 'partial' }],
    })
    expect(deriveLibrarySearch('\u2265', 'en', t).hints).toEqual({
      costs: [{ coverage: 'partial' }],
    })
  })

  it('derives a partial constraint from the unpriced label alone', () => {
    expect(deriveLibrarySearch(i18n.t('documents.unpriced'), 'en', t).hints).toEqual({
      costs: [{ coverage: 'partial' }],
    })
  })

  it('derives a partial constraint and the printed count from "3 <unpriced>"', () => {
    const term = `3 ${i18n.t('documents.unpriced')}`
    expect(deriveLibrarySearch(term, 'en', t).hints).toEqual({
      costs: [{ coverage: 'partial', unpricedCallCount: 3 }],
    })
  })

  it('rejects a malformed partial term that is not a displayed label fragment', () => {
    // The formatter prints `≥ <amount> · <count> <unpricedLabel>`; a term that
    // drops the separator neither spells the count nor matches any displayed
    // fragment, so it derives nothing rather than selecting every partial row.
    const term = `\u2265 1.23 ${i18n.t('documents.unpriced')}`
    expect(deriveLibrarySearch(term, 'en', t).hints).toBeNull()
  })

  it('accepts genuine unpriced-label fragments in both directions', () => {
    // What the operator sees, and a substring of it, name the same partial bucket.
    expect(deriveLibrarySearch('unpriced', 'en', t).hints).toEqual({
      costs: [{ coverage: 'partial' }],
    })
    expect(deriveLibrarySearch(i18n.t('documents.unpriced'), 'en', t).hints).toEqual({
      costs: [{ coverage: 'partial' }],
    })
    // Prose that merely contains the label is not a fragment of it.
    expect(deriveLibrarySearch('notes about unpriced calls', 'en', t).hints).toBeNull()
  })

  it('rejects cost terms whose leftover text does not parse', () => {
    // A currency followed by prose is not a currency-only constraint.
    expect(deriveLibrarySearch('EUR prose', 'en', t).hints).toBeNull()
    // A marker anywhere but the leading position is not a label.
    expect(deriveLibrarySearch('foo \u2265 bar', 'en', t).hints).toBeNull()
    // Unknown trailing text after a full count and label is not the label.
    expect(deriveLibrarySearch('3 unpriced calls extra', 'en', t).hints).toBeNull()
  })

  it('refuses conflicting currency tokens instead of picking one', () => {
    expect(deriveLibrarySearch('USD 1.23 EUR', 'en', t).hints).toBeNull()
  })

  it('rejects a second field separator', () => {
    const label = `${partialLabel('1.23', 'USD', 3, 'en')} \u00b7 extra`
    expect(deriveLibrarySearch(label, 'en', t).hints).toBeNull()
  })

  it('never treats a random three-letter word as a currency', () => {
    expect(deriveLibrarySearch('ABC', 'en', t).hints).toBeNull()
    expect(deriveLibrarySearch('FOO', 'en', t).hints).toBeNull()
  })

  it('maps the unavailable / unknown label to the blocked bucket', () => {
    expect(deriveLibrarySearch('Not available', 'en', t).hints).toEqual({ unavailable: true })
    expect(deriveLibrarySearch('Cost unknown', 'en', t).hints).toEqual({ unavailable: true })
  })

  it('maps readiness and lifecycle labels to their closed values', () => {
    expect(deriveLibrarySearch('blocked', 'en', t).hints).toEqual({ readiness: [false] })
    expect(deriveLibrarySearch('archived', 'en', t).hints).toEqual({ lifecycle: ['archived'] })
  })

  it('leaves a plain name as a text search with no hint', () => {
    const derivation = deriveLibrarySearch('Alpha', 'en', t)
    expect(derivation.search).toBe('Alpha')
    expect(derivation.hints).toBeNull()
    expect(derivation.normalizedSearch).toBeNull()
  })

  it('normalizes a locale-grouped number without a word dictionary', () => {
    expect(normalizeLibrarySearchNumber('1,234', 'en')).toBe('1234')
  })
})

describe('library facet-local search derivation', () => {
  beforeAll(async () => {
    await i18n.changeLanguage('en')
  })

  it('derives only the selected column and never forwards another column', () => {
    expect(deriveLibraryFacetSearch('cost', '12', 'en', t)).toEqual({
      costs: [{ amount: '12.00' }],
    })
    expect(deriveLibraryFacetSearch('documents', '12', 'en', t)).toEqual({ numbers: [12] })
    expect(deriveLibraryFacetSearch('queue', '12', 'en', t)).toEqual({ numbers: [12] })
    expect(deriveLibraryFacetSearch('library', '12', 'en', t)).toBeNull()
    expect(deriveLibraryFacetSearch('workspace', 'Alpha', 'en', t)).toBeNull()
  })

  it.each(['documents', 'calls', 'queue'] as const)(
    'parses safe integer counts without adding unrelated hints in %s',
    (column) => {
      expect(deriveLibraryFacetSearch(column, ' 0 ', 'en', t)).toEqual({ numbers: [0] })
      expect(deriveLibraryFacetSearch(column, '1,234', 'en', t)).toEqual({ numbers: [1234] })
      expect(deriveLibraryFacetSearch(column, '1\u00a0234', 'ru', t)).toEqual({ numbers: [1234] })
      for (const raw of ['', '   ', '0.50', '-1', '9007199254740992', 'USD 12', 'Alpha']) {
        expect(deriveLibraryFacetSearch(column, raw, 'en', t)).toBeNull()
      }
    },
  )

  it.each(['en', 'ru'] as const)('keeps count NULL buckets column-local in %s', (locale) => {
    const translate = i18n.getFixedT(locale)
    for (const key of ['common.notAvailable', 'dataGrid.nullValue'] as const) {
      const raw = translate(key)
      expect(deriveLibraryFacetSearch('documents', raw, locale, translate)).toEqual({
        unavailable: true,
      })
      expect(deriveLibraryFacetSearch('calls', raw, locale, translate)).toEqual({
        unavailable: true,
      })
      expect(deriveLibraryFacetSearch('queue', raw, locale, translate)).toBeNull()
    }
    for (const column of ['documents', 'calls', 'queue'] as const) {
      expect(
        deriveLibraryFacetSearch(column, translate('common.costUnknown'), locale, translate),
      ).toBeNull()
    }
  })

  it('round-trips a cost NULL bucket through the cost column only', () => {
    expect(deriveLibraryFacetSearch('cost', 'Cost unknown', 'en', t)).toEqual({
      unavailable: true,
    })
    // The not-available label belongs to documents/calls, not the cost column.
    expect(deriveLibraryFacetSearch('cost', 'Not available', 'en', t)).toBeNull()
    expect(deriveLibraryFacetSearch('documents', 'Not available', 'en', t)).toEqual({
      unavailable: true,
    })
  })

  it('derives a typed partial cost constraint inside the cost column', () => {
    const label = partialLabel('1234.56', 'USD', 3, 'en')
    expect(deriveLibraryFacetSearch('cost', label, 'en', t)).toEqual({
      costs: [
        { amount: '1234.56', currencyCode: 'USD', coverage: 'partial', unpricedCallCount: 3 },
      ],
    })
  })

  it('derives column-owned enum hints, not the whole-table set', () => {
    expect(deriveLibraryFacetSearch('readiness', 'Ready', 'en', t)).toEqual({ readiness: [true] })
    expect(deriveLibraryFacetSearch('lifecycle', 'Archived', 'en', t)).toEqual({
      lifecycle: ['archived'],
    })
    expect(deriveLibraryFacetSearch('readiness', 'Archived', 'en', t)).toBeNull()
  })

  it('returns null for an empty term', () => {
    expect(deriveLibraryFacetSearch('cost', '   ', 'en', t)).toBeNull()
  })

  it('rejects the same non-cost prose in the cost column as the global search', () => {
    expect(deriveLibraryFacetSearch('cost', 'EUR prose', 'en', t)).toBeNull()
    expect(deriveLibraryFacetSearch('cost', 'foo \u2265 bar', 'en', t)).toBeNull()
    expect(deriveLibraryFacetSearch('cost', 'notes about unpriced calls', 'en', t)).toBeNull()
    expect(deriveLibraryFacetSearch('cost', 'USD 1.23 EUR', 'en', t)).toBeNull()
  })
})
