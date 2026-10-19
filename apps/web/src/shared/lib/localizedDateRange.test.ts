import { describe, expect, it } from 'vitest'

import { parseLocalizedDateRange } from './localizedDateRange'

describe('parseLocalizedDateRange', () => {
  it('parses the exact rendered date/time label including seconds', () => {
    const label = new Intl.DateTimeFormat('ru', {
      year: 'numeric',
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
    }).format(new Date(2026, 7, 2, 10, 0, 0))
    expect(label).toBe('02.08.2026, 10:00:00')
    const range = parseLocalizedDateRange(label, 'ru')
    const min = new Date(2026, 7, 2, 10, 0, 0)
    expect(range).toEqual({
      min: min.toISOString(),
      max: new Date(min.getTime() + 1000).toISOString(),
    })
  })

  it('widens an entered minute to the whole minute and a bare date to the local day', () => {
    const minute = parseLocalizedDateRange('02.08.2026, 10:00', 'ru')
    const at = new Date(2026, 7, 2, 10, 0)
    expect(minute).toEqual({
      min: at.toISOString(),
      max: new Date(at.getTime() + 60_000).toISOString(),
    })
    const day = parseLocalizedDateRange('02.08.2026', 'ru')
    expect(day).toEqual({
      min: new Date(2026, 7, 2).toISOString(),
      max: new Date(2026, 7, 3).toISOString(),
    })
  })

  it('rejects a term this locale could not have produced', () => {
    expect(parseLocalizedDateRange('not a date', 'ru')).toBeUndefined()
    expect(parseLocalizedDateRange('99.99.2026', 'ru')).toBeUndefined()
  })
  it.each([false, true])('preserves English day periods (seconds: %s)', (seconds) => {
    const min = new Date(2026, 7, 2, 13, 5, seconds ? 7 : 0)
    const label = new Intl.DateTimeFormat('en', {
      year: 'numeric',
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      ...(seconds ? { second: '2-digit' } : {}),
      hour12: true,
    }).format(min)
    expect(parseLocalizedDateRange(label, 'en')).toEqual({
      min: min.toISOString(),
      max: new Date(min.getTime() + (seconds ? 1000 : 60_000)).toISOString(),
    })
  })
  it('rejects long whitespace tails and invalid day periods without optional-branch backtracking', () => {
    for (const tail of [' '.repeat(100_000), `${' '.repeat(100_000)}invalid`]) {
      expect(parseLocalizedDateRange(`08/02/2026, 01:05${tail}`, 'en')).toBeUndefined()
      expect(parseLocalizedDateRange(`08/02/2026, 01:05:07${tail}`, 'en')).toBeUndefined()
    }
  })
})
