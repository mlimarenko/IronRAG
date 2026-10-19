import type { Locale } from '@/shared/types'

/** A half-open `[min, max)` instant interval, in ISO 8601. */
export interface LocalizedDateRange {
  min: string
  max: string
}

// Match each displayed shape separately. Whitespace followed by a non-space
// token cannot backtrack into an optional time or day-period branch.
const DATE_ONLY_PATTERN = /^(\d{1,4})[./-](\d{1,2})[./-](\d{1,4})$/
const DATE_TIME_PATTERN = /^(\d{1,4})[./-](\d{1,2})[./-](\d{1,4}),?\s+(\S.*)$/
const TIME_SECOND_PERIOD_PATTERN = /^(\d{1,2}):(\d{2}):(\d{2})\s+(\S.*)$/
const TIME_MINUTE_PERIOD_PATTERN = /^(\d{1,2}):(\d{2})\s+(\S.*)$/
const TIME_SECOND_PATTERN = /^(\d{1,2}):(\d{2}):(\d{2})$/
const TIME_MINUTE_PATTERN = /^(\d{1,2}):(\d{2})$/

function parseDisplayedTime(term: string) {
  const secondsWithPeriod = TIME_SECOND_PERIOD_PATTERN.exec(term)
  if (secondsWithPeriod)
    return {
      hour: Number(secondsWithPeriod[1]),
      minute: Number(secondsWithPeriod[2]),
      second: Number(secondsWithPeriod[3]),
      period: secondsWithPeriod[4],
      hasSeconds: true,
    }
  const minutesWithPeriod = TIME_MINUTE_PERIOD_PATTERN.exec(term)
  if (minutesWithPeriod)
    return {
      hour: Number(minutesWithPeriod[1]),
      minute: Number(minutesWithPeriod[2]),
      second: 0,
      period: minutesWithPeriod[3],
      hasSeconds: false,
    }
  const seconds = TIME_SECOND_PATTERN.exec(term)
  if (seconds)
    return {
      hour: Number(seconds[1]),
      minute: Number(seconds[2]),
      second: Number(seconds[3]),
      period: undefined,
      hasSeconds: true,
    }
  const minutes = TIME_MINUTE_PATTERN.exec(term)
  if (minutes)
    return {
      hour: Number(minutes[1]),
      minute: Number(minutes[2]),
      second: 0,
      period: undefined,
      hasSeconds: false,
    }
  return undefined
}

/**
 * The one localized date/time parser this product has.
 *
 * A reader searches for the value they see. The date column renders through an
 * `Intl.DateTimeFormat`, so the searchable spelling of an instant is derived
 * from the same `Intl` field order and day periods — never from a handwritten
 * language keyword dictionary. It accepts the optional seconds the grid renders
 * and returns the UTC interval for the entered second (or the whole local day
 * when no time is present), or `undefined` when the term is not a date this
 * locale could have produced.
 */
export function parseLocalizedDateRange(
  term: string,
  locale: Locale,
): LocalizedDateRange | undefined {
  const dateFormat = new Intl.DateTimeFormat(locale, { dateStyle: 'short' })
  const sample = new Date(2026, 9, 1, 13, 5)
  const order = dateFormat
    .formatToParts(sample)
    .filter((part) => ['day', 'month', 'year'].includes(part.type))
  const withTime = DATE_TIME_PATTERN.exec(term)
  const match = withTime ?? DATE_ONLY_PATTERN.exec(term)
  if (!match) return undefined
  const time = withTime ? parseDisplayedTime(withTime[4]!) : undefined
  if (withTime && !time) return undefined
  const values = new Map(order.map((part, index) => [part.type, Number(match[index + 1])]))
  const year = values.get('year')!
  if (year < 100) return undefined
  const month = values.get('month')! - 1
  const day = values.get('day')!
  let hour = time?.hour ?? 0
  const minute = time?.minute ?? 0
  const second = time?.second ?? 0
  if (time?.period) {
    const periods = new Intl.DateTimeFormat(locale, { hour: 'numeric', hour12: true })
    const am = periods
      .formatToParts(new Date(2026, 0, 1, 1))
      .find((part) => part.type === 'dayPeriod')?.value
    const pm = periods
      .formatToParts(new Date(2026, 0, 1, 13))
      .find((part) => part.type === 'dayPeriod')?.value
    const entered = time.period.toLocaleLowerCase(locale)
    if (
      hour < 1 ||
      hour > 12 ||
      ![am?.toLocaleLowerCase(locale), pm?.toLocaleLowerCase(locale)].includes(entered)
    )
      return undefined
    hour = (hour % 12) + (entered === pm?.toLocaleLowerCase(locale) ? 12 : 0)
  }
  const min = new Date(year, month, day, hour, minute, second)
  if (
    min.getFullYear() !== year ||
    min.getMonth() !== month ||
    min.getDate() !== day ||
    hour > 23 ||
    minute > 59 ||
    second > 59
  )
    return undefined
  // The upper bound is the next local midnight (a whole day), the next minute
  // (an entered `H:MM`), or the next second (an entered `H:MM:SS`). A day
  // interval is built from the local date, so it follows the DST transition
  // instead of assuming a fixed 24 hours.
  let max: Date
  if (time) {
    const step = time.hasSeconds ? 1000 : 60_000
    max = new Date(min.getTime() + step)
  } else {
    max = new Date(year, month, day + 1)
  }
  return { min: min.toISOString(), max: max.toISOString() }
}
