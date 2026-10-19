import { beforeEach, describe, expect, it } from 'vitest'

import i18n from '@/shared/i18n'
import { formatBytes } from '@/shared/format'
import { documentFacetLabel } from './documentsGrid'

import { deriveDocumentSearch, normalizeDocumentSearchTerm } from './documentsSearch'

describe('normalizeDocumentSearchTerm', () => {
  it('preserves every digit of a locale decimal, including a high-precision cost', () => {
    expect(normalizeDocumentSearchTerm('0,123456789012345678', 'ru')).toBe('0.123456789012345678')
    expect(normalizeDocumentSearchTerm('1234.5', 'en')).toBe('1234.5')
  })

  it('strips the locale grouping separator', () => {
    expect(normalizeDocumentSearchTerm('1,234.5', 'en')).toBe('1234.5')
  })

  it('reverses a byte size in the units the page displays', () => {
    expect(normalizeDocumentSearchTerm('1.5 KB', 'en')).toBe('1536')
    expect(normalizeDocumentSearchTerm('2 MB', 'en')).toBe(String(2 * 1024 ** 2))
  })

  it('returns null for a term it cannot honestly canonicalise', () => {
    // The browser has no locale date/duration parser, so those stay raw.
    expect(normalizeDocumentSearchTerm('01.10.2026', 'en')).toBeNull()
    expect(normalizeDocumentSearchTerm('2m 30s', 'en')).toBeNull()
    expect(normalizeDocumentSearchTerm('handbook', 'en')).toBeNull()
  })
})

describe('deriveDocumentSearch', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('en')
  })

  it('keeps the raw term and adds the derived-status buckets its label matches', () => {
    const derived = deriveDocumentSearch('ready', 'en', i18n.t)
    expect(derived.search).toBe('ready')
    expect(derived.searchStatus).toContain('ready')
  })

  it('matches a localized status label, not just the storage token', async () => {
    await i18n.changeLanguage('ru')
    const derived = deriveDocumentSearch('Готов', 'ru', i18n.t)
    expect(derived.searchStatus).toContain('ready')
  })

  it('carries the normalized decimal alongside the raw term', () => {
    const derived = deriveDocumentSearch('0,50', 'ru', i18n.t)
    expect(derived.search).toBe('0,50')
    expect(derived.normalizedSearch).toBe('0.50')
  })

  it('keeps non-ASCII names raw with no invented normalization', () => {
    const derived = deriveDocumentSearch('Отчёт за март', 'ru', i18n.t)
    expect(derived.search).toBe('Отчёт за март')
    expect(derived.normalizedSearch).toBeNull()
  })

  it('searches every byte size rounded to the displayed value, not just its midpoint', () => {
    for (const size of [1537, 10 * 1024 + 300, 2 * 1024 ** 2 + 1]) {
      const label = formatBytes(size)
      const range = deriveDocumentSearch(label, 'en', i18n.t).searchRanges?.fileSize
      expect(range).toBeDefined()
      expect(size).toBeGreaterThanOrEqual(range!.min)
      expect(size).toBeLessThan(range!.max)
      expect(formatBytes(range!.min)).toBe(label)
      expect(formatBytes(range!.max - 1)).toBe(label)
    }
  })

  it('searches a formatted duration across the whole displayed second', () => {
    expect(deriveDocumentSearch('1m 30s', 'en', i18n.t).searchRanges?.pipelineTime).toEqual({
      min: 90_000,
      max: 91_000,
    })
    expect(deriveDocumentSearch('0s', 'en', i18n.t).searchRanges?.pipelineTime).toEqual({
      min: 0,
      max: 1000,
    })
  })

  it.each(['en', 'ru'] as const)(
    'reverses the actual %s date display into its local minute',
    (locale) => {
      const value = new Date(2026, 9, 1, 13, 5, 47)
      const label = documentFacetLabel('uploaded_at', value.toISOString(), i18n.t, locale)
      expect(deriveDocumentSearch(label, locale, i18n.t).searchRanges?.date).toEqual({
        min: new Date(2026, 9, 1, 13, 5).toISOString(),
        max: new Date(2026, 9, 1, 13, 6).toISOString(),
      })
    },
  )

  it('uses a local-day interval for a date without a time and rejects impossible dates', () => {
    expect(deriveDocumentSearch('01.10.2026', 'ru', i18n.t).searchRanges?.date).toEqual({
      min: new Date(2026, 9, 1).toISOString(),
      max: new Date(2026, 9, 2).toISOString(),
    })
    expect(deriveDocumentSearch('31.02.2026', 'ru', i18n.t).searchRanges).toBeNull()
    expect(deriveDocumentSearch('9007199254740993 GB', 'en', i18n.t).searchRanges).toBeNull()
  })

  it('keeps exact decimal search digits while accepting the displayed currency prefix', () => {
    expect(deriveDocumentSearch('$9007199254740993.000001', 'en', i18n.t).normalizedSearch).toBe(
      '9007199254740993.000001',
    )
  })

  it('parses a locale date-time with a day period into the same local minute', () => {
    const value = new Date(2026, 9, 1, 13, 5, 30)
    const label = documentFacetLabel('uploaded_at', value.toISOString(), i18n.t, 'en')
    expect(label).toMatch(/\d{1,2}:\d{2}\s*(AM|PM)/i)
    expect(deriveDocumentSearch(label, 'en', i18n.t).searchRanges?.date).toEqual({
      min: new Date(2026, 9, 1, 13, 5).toISOString(),
      max: new Date(2026, 9, 1, 13, 6).toISOString(),
    })
  })

  it('rejects a date-time whose clock fields are out of range', () => {
    expect(deriveDocumentSearch('01.10.2026, 25:00', 'ru', i18n.t).searchRanges).toBeNull()
    expect(deriveDocumentSearch('01.10.2026, 12:60', 'ru', i18n.t).searchRanges).toBeNull()
  })

  it('keeps a whole-unit byte display pinned to its unit floor and a fractional one to a tenth', () => {
    expect(deriveDocumentSearch('10 KB', 'en', i18n.t).searchRanges?.fileSize).toEqual({
      min: 10 * 1024,
      max: 10.5 * 1024,
    })
    expect(deriveDocumentSearch('1.5 KB', 'en', i18n.t).searchRanges?.fileSize).toEqual({
      min: 1485,
      max: 1588,
    })
  })
})
