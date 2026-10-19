import type { TFunction } from 'i18next'

import type { DocumentListStatusFilter } from '@/shared/api'
import type { DocumentSearchRanges } from '@/shared/api/generated'
import { documentStatusLabel } from '@/shared/lib/enumLabels'
import { parseLocalizedDateRange } from '@/shared/lib/localizedDateRange'
import type { Locale } from '@/shared/types'

/**
 * The statuses a literal search can resolve to. Kept in one place so the
 * derivation and the status buckets cannot disagree about the closed set.
 */
const DOCUMENT_STATUS_VALUES: readonly DocumentListStatusFilter[] = [
  'ready',
  'processing',
  'queued',
  'failed',
  'canceled',
  'empty',
]

export interface DocumentSearchDerivation {
  /** The raw term, trimmed. Names and non-ASCII survive untouched. */
  search: string
  /** Derived-status buckets whose localized label matches the term. */
  searchStatus: DocumentListStatusFilter[]
  /** An Intl-derived canonical spelling of the term, or null when none applies. */
  normalizedSearch: string | null
  searchRanges: DocumentSearchRanges | null
}

const BYTE_MULTIPLIERS: Record<string, number> = {
  B: 1,
  KB: 1024,
  MB: 1024 ** 2,
  GB: 1024 ** 3,
}

function localeSeparators(locale: string): { group: string; decimal: string } {
  const parts = new Intl.NumberFormat(locale).formatToParts(1234567.89)
  return {
    group: parts.find((part) => part.type === 'group')?.value ?? ',',
    decimal: parts.find((part) => part.type === 'decimal')?.value ?? '.',
  }
}

/**
 * The one canonical spelling this client can derive from the format it
 * displays: a locale decimal (and its grouping separators), and a byte size in
 * the units `formatBytes` emits. The digits of a decimal are preserved exactly,
 * so a cost search never loses precision to a float. Rounded displays are
 * represented separately as typed intervals, not fake exact numeric values.
 */
export function normalizeDocumentSearchTerm(term: string, locale: string): string | null {
  const trimmed = term.trim()
  if (!trimmed) return null
  const { group, decimal } = localeSeparators(locale)
  const ungrouped = trimmed
    .replace(/^\$\s*/, '')
    .split(group)
    .join('')
  const canonicalNumber = group === decimal ? ungrouped : ungrouped.replace(decimal, '.')
  if (/^-?\d+(\.\d+)?$/.test(canonicalNumber)) return canonicalNumber
  const bytes = /^(\d+(?:[.,]\d+)?)\s*(B|KB|MB|GB)$/i.exec(trimmed)
  if (bytes) {
    const amount = Number(bytes[1]!.replace(decimal, '.'))
    const multiplier = BYTE_MULTIPLIERS[bytes[2]!.toUpperCase()]
    if (multiplier !== undefined && Number.isFinite(amount)) {
      return String(Math.round(amount * multiplier))
    }
  }
  return null
}

function byteSearchRange(term: string) {
  const match = /^(\d+(?:\.\d+)?)\s*(B|KB|MB|GB)$/i.exec(term)
  if (!match) return undefined
  const amount = Number(match[1])
  const unit = match[2]!.toUpperCase()
  const multiplier = BYTE_MULTIPLIERS[unit]!
  // A size under ten units, or one written with a decimal digit, is displayed
  // with one fraction digit, so its exact range is half a rounding step wide.
  const fractional = amount < 10 || match[1]!.includes('.')
  const tolerance = fractional ? 0.05 : 0.5
  const step = fractional ? 1 : 10
  const minUnit = unit === 'B' ? 1 : multiplier * step
  const upperUnit = unit === 'GB' ? Number.MAX_SAFE_INTEGER : multiplier * 1024
  const min =
    unit === 'B' ? amount : Math.max(minUnit, Math.ceil((amount - tolerance) * multiplier))
  const precisionUpper = fractional ? 10 * multiplier : upperUnit
  const max =
    unit === 'B'
      ? amount + 1
      : Math.min(upperUnit, precisionUpper, Math.ceil((amount + tolerance) * multiplier))
  return Number.isSafeInteger(min) && Number.isSafeInteger(max) && max > min
    ? { min, max }
    : undefined
}

function durationSearchRange(term: string) {
  const match = /^(?:(\d+)m\s+)?(\d+)s$/.exec(term)
  if (!match) return undefined
  const min = (Number(match[1] ?? 0) * 60 + Number(match[2])) * 1000
  return Number.isSafeInteger(min + 1000) ? { min, max: min + 1000 } : undefined
}

/** Date/time spelling comes from the shared Intl parser. */
function dateSearchRange(term: string, locale: Locale) {
  return parseLocalizedDateRange(term, locale)
}

/**
 * One whole-table search derived once for the server: the raw term (names,
 * non-ASCII, exact decimals), the derived-status buckets whose localized label
 * contains it, and an Intl-derived canonical spelling. No handwritten language
 * keyword dictionary is involved — every label comes from the enum label
 * formatter and every separator from `Intl`.
 */
export function deriveDocumentSearch(
  raw: string,
  locale: Locale,
  t: TFunction,
): DocumentSearchDerivation {
  const search = raw.trim()
  if (!search) return { search: '', searchStatus: [], normalizedSearch: null, searchRanges: null }
  const needle = search.toLocaleLowerCase(locale)
  const searchStatus = DOCUMENT_STATUS_VALUES.filter((status) =>
    documentStatusLabel(status, t).toLocaleLowerCase(locale).includes(needle),
  )
  const fileSize = byteSearchRange(search)
  const pipelineTime = durationSearchRange(search)
  const date = dateSearchRange(search, locale)
  const searchRanges =
    fileSize || pipelineTime || date
      ? {
          ...(fileSize ? { fileSize } : {}),
          ...(pipelineTime ? { pipelineTime } : {}),
          ...(date ? { date } : {}),
        }
      : null
  return {
    search,
    searchStatus,
    searchRanges,
    normalizedSearch: normalizeDocumentSearchTerm(search, locale),
  }
}
