// Cost roll-up shape shared by the documents and admin cost surfaces. Mirrors
// the backend cost envelope (see costCoverage.ts for the coverage semantics).
// `costRollupFreshness` is a *freshness* fact, independent of the pricing
// `costCoverage`: while `reconciling` the backend withholds every value field,
// so each value is optional/null and a missing one is "not known yet", never
// zero.
export type CostSummary = {
  costRollupFreshness: 'current' | 'reconciling'
  totalCost?: string | null
  currencyCode?: string | null
  providerCallCount?: number | null
  unpricedProviderCallCount?: number | null
  costCoverage?: 'complete' | 'partial' | 'unknown' | null
}

export function formatNumber(value: number, locale = 'en') {
  return new Intl.NumberFormat(locale).format(value)
}

export function formatDate(value: string, locale = 'en', options?: Intl.DateTimeFormatOptions) {
  return new Intl.DateTimeFormat(
    locale,
    options ?? { dateStyle: 'medium', timeStyle: 'short' },
  ).format(new Date(value))
}

/// Renders an instant as "5 minutes ago", flooring anything under a minute to
/// `justNowLabel`.
///
/// Second-level output re-renders with every poll: the dashboard refreshes its
/// aggregate every 2.5s and stamps `recomputedAt` server-side, so the label
/// counted seconds and flipped between past and future on the smallest clock
/// skew between the browser and the API. A minute floor is the coarsest unit
/// that never lies about the value it shows, and it makes the label stand
/// still.
export function formatRelative(value: string, locale: string, justNowLabel: string) {
  const seconds = Math.round((new Date(value).getTime() - Date.now()) / 1000)
  if (Math.abs(seconds) < 60) return justNowLabel
  const units: Array<[Intl.RelativeTimeFormatUnit, number]> = [
    ['day', 86400],
    ['hour', 3600],
    ['minute', 60],
  ]
  const [unit, size] = units.find(([, threshold]) => Math.abs(seconds) >= threshold) ?? [
    'minute',
    60,
  ]
  return new Intl.RelativeTimeFormat(locale, { numeric: 'auto' }).format(
    Math.round(seconds / size),
    unit,
  )
}

/** Publication size: absence is not a measured zero-byte file. */
export function formatDocumentSize(fileSize: number | null | undefined): string {
  if (fileSize == null) return '—'
  if (fileSize === 0) return '0 B'
  return formatBytes(fileSize)
}

export function formatBytes(value: number) {
  // Connector-ingested documents carry no byte size; "0 B" reads as a broken
  // file rather than "unknown", so render nothing measurable instead.
  if (!Number.isFinite(value) || value <= 0) return ''
  if (value < 1024) return `${value} B`
  const units = ['KB', 'MB', 'GB']
  let size = value / 1024
  let unit = units[0]!
  for (let index = 1; size >= 1024 && index < units.length; index += 1) {
    size /= 1024
    unit = units[index]!
  }
  return `${size.toFixed(size >= 10 ? 0 : 1)} ${unit}`
}

export function formatCost(
  summary: CostSummary | null,
  unknownLabel: string,
  unpricedLabel: string,
  locale: string,
  maximumFractionDigits = 2,
) {
  // A reconciling read carries no exact amount; an incomplete current read is
  // "unknown". Neither may render as a fabricated zero.
  if (!summary || summary.costRollupFreshness === 'reconciling') return unknownLabel
  if (!summary.totalCost || !summary.currencyCode || summary.costCoverage === 'unknown')
    return unknownLabel
  const amount = formatCurrencyAmount(
    summary.totalCost,
    summary.currencyCode,
    locale,
    maximumFractionDigits,
  )
  if (summary.costCoverage === 'partial')
    return `\u2265 ${amount} \u00b7 ${summary.unpricedProviderCallCount ?? 0} ${unpricedLabel}`
  return amount
}

/**
 * The one formula behind every cost amount this UI prints. The number is always
 * two decimal places, half-up, computed from the exact decimal source string
 * through `Intl` with an explicit currency — never `Number(...)` of the raw cost
 * and never a page-locale float. `maximumFractionDigits` lets the model-price
 * surface keep its finer precision without a second formatter. `currencyDisplay:
 * 'code'` keeps a non-USD amount unambiguous (never a bare `$`), and the
 * currency code the server reported is always printed, never a guessed one.
 */
export function formatCurrencyAmount(
  amount: string,
  currencyCode: string,
  locale: string,
  maximumFractionDigits = 2,
): string {
  const numeric = new Intl.NumberFormat(locale, {
    style: 'currency',
    currency: currencyCode,
    currencyDisplay: 'code',
    maximumFractionDigits,
    // ECMA-402 formats decimal strings without binary-float conversion. TypeScript
    // still declares only number/bigint here; this assertion changes no runtime value.
  }).format(amount as unknown as number)
  // Intl may use a non-breaking space; keep the reader's dimensions stable.
  return numeric.replaceAll('\u00a0', ' ')
}

/** Join metadata fragments with a dot separator, skipping empty ones. */
export function metaLine(...parts: Array<string | null | undefined>): string {
  return parts.filter((part): part is string => Boolean(part?.trim())).join(' · ')
}
