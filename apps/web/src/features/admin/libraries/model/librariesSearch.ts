import type { TFunction } from 'i18next'

import type {
  AdminLibraryCostSearch,
  AdminLibraryFacetColumn,
  AdminLibrarySearchHints,
} from '@/shared/api/generated'
import { libraryLifecycleStateLabel } from '@/shared/lib/enumLabels'
import type { Locale } from '@/shared/types'

/**
 * The closed, server-owned value sets whose localized label a term can name.
 * Kept beside the derivation so the label it reads and the value it sends come
 * from the same list.
 */
const READINESS_VALUES: readonly boolean[] = [true, false]
const LIFECYCLE_VALUES: readonly string[] = ['active', 'archived']
/**
 * The localized labels the blocked columns show, never a hand-written keyword
 * list. The shared menu renders every NULL option with `dataGrid.nullValue`;
 * the cells also show `common.costUnknown` or `common.notAvailable`. Read both
 * representations only in the columns that own an unavailable bucket.
 */
const GRID_NULL_LABEL_KEY = 'dataGrid.nullValue'
const COST_UNKNOWN_LABEL_KEYS = ['common.costUnknown', GRID_NULL_LABEL_KEY] as const
const NOT_AVAILABLE_LABEL_KEYS = ['common.notAvailable', GRID_NULL_LABEL_KEY] as const
const UNAVAILABLE_LABEL_KEYS = [...COST_UNKNOWN_LABEL_KEYS, ...NOT_AVAILABLE_LABEL_KEYS] as const

/** The formatter's partial marker and its field separator, read structurally. */
const PARTIAL_MARKER = '\u2265'
const FIELD_SEPARATOR = '\u00b7'

/**
 * The platform's own closed ISO 4217 table. `Intl.NumberFormat` accepts any
 * three-letter token (`ABC`, `FOO`), so the formatter's currency slot cannot
 * itself prove an ISO code; `Intl.supportedValuesOf('currency')` is the
 * standard list of exactly the currencies the platform recognises, so no
 * hand-written currency dictionary is needed.
 */
const ISO_CURRENCY_CODES: ReadonlySet<string> = new Set(Intl.supportedValuesOf('currency'))

export interface LibrarySearchDerivation {
  /** The raw term, trimmed. Names and non-ASCII survive untouched. */
  search: string
  /** An Intl-derived canonical number spelling, or null when none applies. */
  normalizedSearch: string | null
  /** Typed hints over the non-text columns; null when the term names none. */
  hints: AdminLibrarySearchHints | null
}

function localeSeparators(locale: string): { group: string; decimal: string } {
  const parts = new Intl.NumberFormat(locale).formatToParts(1234567.89)
  return {
    group: parts.find((part) => part.type === 'group')?.value ?? ',',
    decimal: parts.find((part) => part.type === 'decimal')?.value ?? '.',
  }
}

/** Whether `token` is one of the platform's own ISO 4217 currency codes. */
function isIsoCurrencyCode(token: string): boolean {
  return /^[A-Za-z]{3}$/.test(token) && ISO_CURRENCY_CODES.has(token.toUpperCase())
}

/** One parsed displayed amount expression: an amount, a currency, or both. */
interface AmountExpression {
  /** The canonical rounded display amount, when the term names one. */
  amount: string | null
  /** The canonical unrounded decimal the term spells, for the text spelling. */
  raw: string | null
  /** The displayed ISO currency code, when the term names one. */
  currencyCode: string | null
  /** Fraction digits the amount carried, to tell a bare integer from a decimal. */
  fractionDigits: number
}

/**
 * Parse one displayed amount expression. The formatter prints either
 * `<code> <amount>` (en) or `<amount> <code>` (ru), with an optional single
 * leading or trailing ISO code; an amount or a code alone is a genuine label
 * fragment. Anything else is rejected: a token that is not the recognised
 * currency must still parse as the amount, a conflicting leading *and* trailing
 * code (`USD 1.23 EUR`) is refused rather than silently picking one, and no
 * unrecognised remainder survives (`EUR prose` is not a currency).
 */
function parseAmountExpression(segment: string, locale: string): AmountExpression | null {
  let value = segment.trim()
  if (!value) return null
  let leadingCurrency: string | null = null
  const leading = /^([A-Za-z]{3})(?=\s|$)/.exec(value)
  if (leading && isIsoCurrencyCode(leading[1]!)) {
    leadingCurrency = leading[1]!.toUpperCase()
    value = value.slice(3).trim()
  }
  let trailingCurrency: string | null = null
  const trailing = /(?:\s|^)([A-Za-z]{3})$/u.exec(value)
  if (trailing && isIsoCurrencyCode(trailing[1]!)) {
    trailingCurrency = trailing[1]!.toUpperCase()
    value = value.slice(0, -3).trim()
  }
  if (leadingCurrency && trailingCurrency) return null
  const currencyCode = leadingCurrency ?? trailingCurrency
  if (!value) {
    return currencyCode ? { amount: null, raw: null, currencyCode, fractionDigits: 0 } : null
  }
  const parsed = parseCanonicalDecimal(value, locale)
  if (!parsed) return null
  return {
    amount: roundHalfUpToTwo(parsed.value),
    raw: parsed.value,
    currencyCode,
    fractionDigits: parsed.fractionDigits,
  }
}

function parseCanonicalDecimal(
  term: string,
  locale: string,
): { value: string; fractionDigits: number } | null {
  const { group, decimal } = localeSeparators(locale)
  // Intl may emit a non-breaking or narrow no-break space as the group
  // separator; accept every space-like variant so the parser round-trips the
  // formatter's own output regardless of which the platform chose.
  const normalized = term.replace(/[\u00a0\u202f\u2009]/gu, ' ')
  const ungrouped = normalized.split(group === '\u00a0' ? ' ' : group).join('')
  const canonical = group === decimal ? ungrouped : ungrouped.replace(decimal, '.')
  if (!/^\d+(\.\d+)?$/.test(canonical)) return null
  const fractionDigits = canonical.includes('.') ? (canonical.split('.')[1]?.length ?? 0) : 0
  return { value: canonical, fractionDigits }
}

/**
 * Round a non-negative decimal string to two fraction digits, half-up, using
 * integer arithmetic only: the cost column's canonical display precision, so a
 * search term and the value it matches lose no more than the display does.
 * No `Number(...)` ever touches the raw decimal.
 */
function roundHalfUpToTwo(value: string): string {
  const [intPart = '0', fracPart = ''] = value.split('.')
  const padded = `${fracPart}00`
  const whole = BigInt(intPart || '0') * 100n + BigInt(padded.slice(0, 2) || '0')
  const carry = BigInt(padded.slice(2, 3) || '0') >= 5n ? 1n : 0n
  const rounded = whole + carry
  return `${rounded / 100n}.${(rounded % 100n).toString().padStart(2, '0')}`
}

/** The integer the term spells, or null when it is not a bare safe integer. */
function deriveNumberHint(term: string, locale: string): number | null {
  const parsed = parseCanonicalDecimal(term.trim(), locale)
  if (!parsed || parsed.fractionDigits > 0) return null
  const numeric = Number(parsed.value)
  // A grouped decimal beyond 2^53 would silently change under `Number`; refuse it.
  return Number.isSafeInteger(numeric) && String(numeric) === parsed.value ? numeric : null
}

/**
 * Parse a label tail — the optional displayed count and the unpriced-label
 * fragment. The fragment is matched in the same direction as the enum labels:
 * the displayed label must *contain* what the operator typed, so `unpriced` and
 * `unpriced calls` are genuine fragments while `notes about unpriced calls` is
 * not. `allowCountOnly` admits the separator form's `<count>` prefix, which the
 * formatter always prints before the label.
 */
function parseLabelTail(
  text: string,
  labelLower: string,
  locale: Locale,
  allowCountOnly: boolean,
): { count: number | null } | null {
  const rest = text.trim()
  if (rest === '') return allowCountOnly ? { count: null } : null
  const withLabel = /^(\d+)\s+(\S.*)$/u.exec(rest)
  if (withLabel) {
    const numeric = Number(withLabel[1])
    if (!Number.isSafeInteger(numeric) || numeric < 0) return null
    const fragment = withLabel[2]!.trim()
    return labelLower.includes(fragment.toLocaleLowerCase(locale)) ? { count: numeric } : null
  }
  if (/^\d+$/u.test(rest)) {
    if (!allowCountOnly) return null
    const numeric = Number(rest)
    return Number.isSafeInteger(numeric) && numeric >= 0 ? { count: numeric } : null
  }
  return labelLower.includes(rest.toLocaleLowerCase(locale)) ? { count: null } : null
}

/** The cost constraint a `<marker?> <amount> · <count?> <label?>` partial names. */
function derivePartialCost(
  expression: AmountExpression | null,
  tail: { count: number | null } | null,
): AdminLibraryCostSearch {
  const search: AdminLibraryCostSearch = { coverage: 'partial' }
  if (expression?.amount) search.amount = expression.amount
  if (expression?.currencyCode) search.currencyCode = expression.currencyCode
  if (tail?.count !== null && tail?.count !== undefined) search.unpricedCallCount = tail.count
  return search
}

/** A leading partial marker can stand alone or qualify an amount or label tail. */
function deriveMarkedCost(
  rest: string,
  labelLower: string,
  locale: Locale,
): AdminLibraryCostSearch | null {
  if (rest === '') return { coverage: 'partial' }
  const expression = parseAmountExpression(rest, locale)
  if (expression) return derivePartialCost(expression, null)
  const tail = parseLabelTail(rest, labelLower, locale, false)
  return tail ? derivePartialCost(null, tail) : null
}

/** A currency and amount spell complete coverage; either alone leaves it unconstrained. */
function deriveAmountCost(expression: AmountExpression | null): AdminLibraryCostSearch | null {
  if (!expression) return null
  if (expression.amount === null) {
    return expression.currencyCode ? { currencyCode: expression.currencyCode } : null
  }
  const search: AdminLibraryCostSearch = { amount: expression.amount }
  if (expression.currencyCode) {
    search.currencyCode = expression.currencyCode
    search.coverage = 'complete'
  }
  return search
}

/**
 * The typed cost constraint one displayed cost term carries, accepted only in
 * the bounded lexical forms the shared formatter actually prints (plus their
 * genuine substrings):
 *
 *   A                 an amount with a decimal fraction  -> `{amount}`
 *   C A / A C         currency and amount                -> `{amount,currency,coverage:'complete'}`
 *   C                 a currency alone                   -> `{currency}`
 *   ≥ [A]             a leading partial marker           -> `{amount?,currency?,coverage:'partial'}`
 *   [≥] A · [N] [L]   marker, amount, count and label    -> partial with each parsed piece
 *   [N] L / L         a genuine label fragment           -> `{coverage:'partial',unpricedCallCount?}`
 *
 * Every other term is rejected. The marker must lead (`foo ≥ bar` is not a
 * label), a separator appears at most once, conflicting currency codes are
 * refused, an unrecognised remainder after a code is not a currency (`EUR prose`),
 * and prose that merely contains the label (`notes about unpriced calls`) is not
 * a fragment. Coverage follows the displayed form: a complete currency+amount
 * has no `≥` suffix so it constrains `coverage: 'complete'` and never matches a
 * partial row with an identical amount; an amount without a currency matches
 * every coverage; a partial term constrains `coverage: 'partial'`.
 */
function deriveCostHints(
  term: string,
  locale: Locale,
  t: TFunction,
): AdminLibraryCostSearch | null {
  const trimmed = term.trim()
  if (!trimmed) return null
  const labelLower = t('documents.unpriced').toLocaleLowerCase(locale)
  const startsWithMarker = trimmed.startsWith(PARTIAL_MARKER)
  const hasMarker = trimmed.includes(PARTIAL_MARKER)
  const segments = trimmed.split(FIELD_SEPARATOR)
  const hasSeparator = segments.length > 1

  if (hasMarker && !startsWithMarker) return null
  if (segments.length > 2) return null

  if (hasSeparator) {
    const left = segments[0]!.trim().replace(PARTIAL_MARKER, ' ').trim()
    const expression = parseAmountExpression(left, locale)
    if (!expression) return null
    const tail = parseLabelTail(segments[1]!, labelLower, locale, true)
    if (!tail) return null
    return derivePartialCost(expression, tail)
  }

  if (startsWithMarker) {
    return deriveMarkedCost(trimmed.slice(PARTIAL_MARKER.length).trim(), labelLower, locale)
  }

  // No marker: the term is either a genuine unpriced-label fragment (optionally
  // led by the printed count) or a complete-form amount expression.
  const labelTail = parseLabelTail(trimmed, labelLower, locale, false)
  if (labelTail) return derivePartialCost(null, labelTail)

  return deriveAmountCost(parseAmountExpression(trimmed, locale))
}

/** Whether any of the column's own labels contains the term. */
function labelMatches(
  needle: string,
  locale: Locale,
  keys: readonly string[],
  t: TFunction,
): boolean {
  return keys.some((key) => t(key).toLocaleLowerCase(locale).includes(needle))
}

function matchingReadiness(needle: string, locale: Locale, t: TFunction): boolean[] {
  return READINESS_VALUES.filter((value) =>
    (value ? t('admin.libraries.ready') : t('admin.libraries.blocked'))
      .toLocaleLowerCase(locale)
      .includes(needle),
  )
}

function matchingLifecycle(needle: string, locale: Locale, t: TFunction): string[] {
  return LIFECYCLE_VALUES.filter((state) =>
    libraryLifecycleStateLabel(state, t).toLocaleLowerCase(locale).includes(needle),
  )
}

/** Count columns share integer parsing; only documents and calls can be unavailable. */
function deriveCountFacetSearch(
  column: 'documents' | 'calls' | 'queue',
  search: string,
  locale: Locale,
  t: TFunction,
): AdminLibrarySearchHints | null {
  const number = deriveNumberHint(search, locale)
  const hints: AdminLibrarySearchHints = {}
  if (number !== null) hints.numbers = [number]
  if (
    column !== 'queue' &&
    labelMatches(search.toLocaleLowerCase(locale), locale, NOT_AVAILABLE_LABEL_KEYS, t)
  ) {
    hints.unavailable = true
  }
  return Object.keys(hints).length > 0 ? hints : null
}

/**
 * One whole-table search derived once for the server: the raw term over the
 * text columns, an Intl-derived canonical number spelling, and the typed hints
 * whose displayed value the term spells — the numbers a documents/queue/calls
 * label formats, the typed cost constraints a cost label carries, whether the
 * term names the unavailable label the blocked columns show, and the readiness /
 * lifecycle values whose localized label contains it. No handwritten language
 * keyword dictionary is involved: every label comes from the page's own label
 * formatter and every separator/symbol from `Intl`.
 */
export function deriveLibrarySearch(
  raw: string,
  locale: Locale,
  t: TFunction,
): LibrarySearchDerivation {
  const search = raw.trim()
  if (!search) return { search: '', normalizedSearch: null, hints: null }
  const needle = search.toLocaleLowerCase(locale)
  const expression = parseAmountExpression(search, locale)

  const costHint = deriveCostHints(search, locale, t)
  const numberHint = deriveNumberHint(search, locale)
  const unavailable = labelMatches(needle, locale, UNAVAILABLE_LABEL_KEYS, t)
  const readiness = matchingReadiness(needle, locale, t)
  const lifecycle = matchingLifecycle(needle, locale, t)
  const hints: AdminLibrarySearchHints = {}
  if (numberHint !== null) hints.numbers = [numberHint]
  if (costHint) hints.costs = [costHint]
  if (unavailable) hints.unavailable = true
  if (readiness.length > 0) hints.readiness = readiness
  if (lifecycle.length > 0) hints.lifecycle = lifecycle
  return {
    search,
    normalizedSearch: expression?.raw && expression.raw !== search ? expression.raw : null,
    hints: Object.keys(hints).length > 0 ? hints : null,
  }
}

/**
 * The typed hints one facet column's own menu search derives, scoped to exactly
 * that column: `cost` carries typed cost constraints and the cost-unknown
 * bucket, `documents`/`calls` carry numbers and their not-available bucket,
 * `queue` carries numbers, `readiness` carries booleans, `lifecycle` carries
 * states, and `library`/`workspace` are text-only. Independently derived from
 * the column's raw term, never a copy of the whole-table hints.
 */
export function deriveLibraryFacetSearch(
  column: AdminLibraryFacetColumn,
  raw: string,
  locale: Locale,
  t: TFunction,
): AdminLibrarySearchHints | null {
  const search = raw.trim()
  if (!search) return null
  const needle = search.toLocaleLowerCase(locale)
  const hints: AdminLibrarySearchHints = {}

  switch (column) {
    case 'cost': {
      const cost = deriveCostHints(search, locale, t)
      if (cost) hints.costs = [cost]
      if (labelMatches(needle, locale, COST_UNKNOWN_LABEL_KEYS, t)) hints.unavailable = true
      break
    }
    case 'documents':
    case 'calls':
    case 'queue':
      return deriveCountFacetSearch(column, search, locale, t)
    case 'readiness': {
      const readiness = matchingReadiness(needle, locale, t)
      if (readiness.length > 0) hints.readiness = readiness
      break
    }
    case 'lifecycle': {
      const lifecycle = matchingLifecycle(needle, locale, t)
      if (lifecycle.length > 0) hints.lifecycle = lifecycle
      break
    }
    default:
      return null
  }

  return Object.keys(hints).length > 0 ? hints : null
}

/**
 * The one canonical numeric spelling this client can derive from what it
 * displays: a locale decimal with its grouping separator, and an optional
 * leading/trailing ISO currency token. The digits are preserved exactly.
 */
export function normalizeLibrarySearchNumber(term: string, locale: string): string | null {
  return parseAmountExpression(term, locale)?.raw ?? null
}
