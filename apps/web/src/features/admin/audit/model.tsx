import type { TFunction } from 'i18next'

import type {
  AuditColumnFilters,
  AuditFacetValue,
  AuditGridColumnId,
  AuditSearchHints,
} from '@/shared/api'
import type { AuditEventPageResponse, AuditEventResponse } from '@/shared/api/generated'
import {
  gridFacetKey,
  gridFacetValue,
  type GridColumn,
  type GridFacetOption,
  type GridState,
} from '@/shared/data-grid'
import { formatCurrencyAmount, formatDate } from '@/shared/format'
import { parseLocalizedDateRange } from '@/shared/lib/localizedDateRange'
import type { Locale } from '@/shared/types'
import { Badge } from '@/shared/ui'

/**
 * The audit grid columns. Ids are the server's own vocabulary — the `sortBy`
 * values, the `facetColumn` values and the query-parameter names all agree on
 * `actor`, `action`, `surface`, `result` and `created_at` — so a column id maps
 * to a facet request and a server sort without a lookup table that could drift.
 */
const AUDIT_GRID_COLUMNS: readonly AuditGridColumnId[] = [
  'actor',
  'action',
  'surface',
  'result',
  'created_at',
]

const GRID_COLUMN_IDS = new Set<string>(AUDIT_GRID_COLUMNS)

/** The one localized formatting for a whole timestamp, shared by cells and facets. */
const TIMESTAMP_FORMAT: Intl.DateTimeFormatOptions = {
  year: 'numeric',
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
}

export type AuditEvent = Readonly<{
  actionKind: string
  actor: string
  actorPrincipalId: string | null
  assistantCall: AuditEventResponse['assistantCall']
  createdAt: string
  id: string
  internalMessage: string | null
  libraryIds: string[]
  redactedMessage: string
  requestId: string | null
  resultKind: string
  subjectSummary: string
  surfaceKind: string
  traceId: string | null
}>

export type AuditPage = Readonly<{
  items: AuditEvent[]
  nextCursor: string | null
  total: number
}>

/**
 * The displayed actor label. It is deliberately the same canonical form the
 * server orders and facets by (display label, else display name, else login,
 * else principal id), so a reader who sees a label can sort and search for the
 * same string. The principal id stays the filter value; the two are not
 * equivalent, so the grid keeps both.
 */
function actorLabel(raw: AuditEventResponse): string {
  const actor = raw.actorPrincipal
  if (!actor) return raw.actorPrincipalId ?? ''
  return actor.displayLabel.trim() || actor.displayName?.trim() || actor.login?.trim() || actor.id
}

export function mapAuditPage(raw: AuditEventPageResponse): AuditPage {
  return {
    items: raw.items.map((event) => ({
      actionKind: event.actionKind,
      actor: actorLabel(event),
      actorPrincipalId: event.actorPrincipalId ?? null,
      assistantCall: event.assistantCall,
      createdAt: event.createdAt,
      id: event.id,
      internalMessage: event.internalMessage ?? null,
      libraryIds: Array.from(
        new Set(
          event.subjects.flatMap((subject) => (subject.libraryId ? [subject.libraryId] : [])),
        ),
      ),
      redactedMessage: event.redactedMessage ?? event.actionKind,
      requestId: event.requestId ?? null,
      resultKind: event.resultKind,
      subjectSummary:
        event.subjects.map((subject) => `${subject.subjectKind}:${subject.subjectId}`).join(', ') ||
        '—',
      surfaceKind: event.surfaceKind,
      traceId: event.traceId ?? null,
    })),
    nextCursor: raw.nextCursor ?? null,
    total: raw.total,
  }
}

export function localizeAuditAction(actionKind: string, t: TFunction): string {
  const key = `admin.auditActionLabels.${actionKind}`
  const label = t(key)
  if (label !== key) return label

  return actionKind
    .split('.')
    .filter(Boolean)
    .map((segment) => {
      const words = segment.replaceAll(/[_-]+/g, ' ')
      return segment.length <= 4
        ? words.toUpperCase()
        : words.replace(/^./, (value) => value.toUpperCase())
    })
    .join(' ')
}

function normalizeAuditText(value: string): string {
  return value
    .toLowerCase()
    .replaceAll(/[._-]+/g, ' ')
    .replaceAll(/\s+/g, ' ')
    .trim()
}

export function shouldShowAuditMessage(event: AuditEvent): boolean {
  if (event.resultKind !== 'succeeded') return true

  // normalizeAuditText has already collapsed whitespace runs to one space and
  // trimmed, so a single \s here matches everything \s+ used to.
  const message = normalizeAuditText(event.redactedMessage).replace(
    /\s(?:succeeded|success|completed)$/,
    '',
  )
  const action = normalizeAuditText(event.actionKind)
  const actionSegments = action.split(' ')
  const actionWithoutDomain = actionSegments.slice(1).join(' ')
  return message !== action && message !== actionWithoutDomain
}

export function compactAuditSubject(subjectSummary: string): string | null {
  if (!subjectSummary || subjectSummary === '—') return null

  return subjectSummary
    .split(',')
    .map((subject) => {
      const trimmed = subject.trim()
      const separator = trimmed.indexOf(':')
      if (separator < 0) return trimmed
      const kind = trimmed.slice(0, separator)
      const id = trimmed.slice(separator + 1)
      const compactId = id.length > 8 ? `${id.slice(0, 8)}…` : id
      return `${kind} · ${compactId}`
    })
    .join(', ')
}

/** A localized contract-enum label; a dynamic key carries the raw value as its fallback. */
function enumLabel(t: TFunction, namespace: string, value: string): string {
  return t(`admin.${namespace}.${value}`, { defaultValue: value })
}

/** The one closed null token every surface reads: the row cell, the facet menu and the chip. */
function auditNullLabel(t: TFunction): string {
  return t('dataGrid.nullValue')
}

/**
 * The human label of one facet value, typed by column. Dates go through the one
 * `TIMESTAMP_FORMAT`, the actor uses the server's canonical label, and the enum
 * columns use their contract labels: no natural-language keyword matching.
 */
export function auditFacetLabel(
  column: AuditGridColumnId,
  value: string | null,
  serverLabel: string,
  t: TFunction,
  locale: Locale,
): string {
  if (value === null) return auditNullLabel(t)
  switch (column) {
    case 'actor':
      return serverLabel
    case 'action':
      return localizeAuditAction(value, t)
    case 'surface':
      return enumLabel(t, 'auditSurfaceLabels', value)
    case 'result':
      return enumLabel(t, 'auditResultLabels', value)
    case 'created_at':
      return formatDate(value, locale, TIMESTAMP_FORMAT)
    default:
      return value
  }
}

/** Whether a selection list carries the null bucket, typed (`null` key) — never the string "null". */

/** Server facet rows -> typed grid options. The key is derived here, never trusted from the wire. */
export function auditFacetOptions(
  column: AuditGridColumnId,
  values: readonly AuditFacetValue[],
  t: TFunction,
  locale: Locale,
): GridFacetOption[] {
  return values.map((row) => {
    const value = row.value ?? null
    return {
      key: gridFacetKey(value),
      value,
      label: auditFacetLabel(column, value, row.label, t, locale),
      ...(value === null ? {} : { title: value }),
      count: row.count,
    }
  })
}

/** One selected value's resolved label, keyed by its canonical typed key. */
export type AuditSelectedLabel = (columnId: AuditGridColumnId, key: string) => string | undefined

/**
 * Options for the active filters of a column, so the toolbar chip reads the
 * same translated/formatted label the menu offered. An actor selection is a
 * principal id, whose label is not derivable here — `resolveLabel` supplies it
 * from the bounded, scoped selected-value metadata request; while that request
 * is in flight the caller renders a pending state rather than the raw UUID.
 */
export function auditSelectedFacets(
  filters: GridState['filters'],
  t: TFunction,
  locale: Locale,
  resolveLabel?: AuditSelectedLabel,
): Record<string, GridFacetOption[]> {
  const result: Record<string, GridFacetOption[]> = {}
  for (const [columnId, keys] of Object.entries(filters)) {
    if (keys.length === 0) continue
    if (!GRID_COLUMN_IDS.has(columnId)) continue
    const column = columnId as AuditGridColumnId
    result[columnId] = keys.map((key) => {
      const decoded = gridFacetValue(key)
      const value = typeof decoded === 'boolean' ? null : decoded
      const resolved = resolveLabel?.(column, key)
      const label =
        resolved ??
        auditFacetLabel(
          column,
          typeof value === 'string' ? value : null,
          typeof value === 'string' ? value : '',
          t,
          locale,
        )
      return { key, value, label, count: -1 }
    })
  }
  return result
}

export function auditGridColumns({
  locale,
  t,
  showInternal,
  showAssistant,
}: AuditGridColumnOptions): GridColumn<AuditEvent>[] {
  return [
    {
      id: 'actor',
      label: t('admin.auditActor'),
      value: (event) => event.actorPrincipalId ?? null,
      // The null actor row and its menu option read the same null token, so a
      // row, a facet value and a chip all say the same thing.
      displayText: (event) => (event.actorPrincipalId === null ? auditNullLabel(t) : event.actor),
    },
    {
      id: 'action',
      label: t('admin.auditAction'),
      value: (event) => event.actionKind,
      cell: (event) => (
        <div className="cell-title">
          <strong>{localizeAuditAction(event.actionKind, t)}</strong>
          {shouldShowAuditMessage(event) ? <p>{event.redactedMessage}</p> : null}
          {compactAuditSubject(event.subjectSummary) ? (
            <small title={event.subjectSummary}>{compactAuditSubject(event.subjectSummary)}</small>
          ) : null}
          {showAssistant && event.assistantCall ? (
            <>
              <small>
                {event.assistantCall.models.length > 0
                  ? event.assistantCall.models
                      .map((model) => `${model.providerKind}:${model.modelName}`)
                      .join(', ')
                  : t('admin.auditAssistantNoModel')}
              </small>
              <small>
                {t('admin.auditAssistantMeta', {
                  count: event.assistantCall.providerCallCount,
                  cost:
                    event.assistantCall.totalCost != null && event.assistantCall.currencyCode
                      ? formatCurrencyAmount(
                          event.assistantCall.totalCost,
                          event.assistantCall.currencyCode,
                          locale,
                          4,
                        )
                      : t('admin.auditAssistantCostUnavailable'),
                })}
              </small>
            </>
          ) : null}
          {showInternal ? (
            <code>
              request_id={event.requestId ?? '—'} · trace_id={event.traceId ?? '—'}
              {event.internalMessage ? ` · ${event.internalMessage}` : ''}
            </code>
          ) : null}
        </div>
      ),
      displayText: (event) => localizeAuditAction(event.actionKind, t),
    },
    {
      id: 'surface',
      label: t('admin.auditSurface'),
      value: (event) => event.surfaceKind,
      cell: (event) => <Badge>{enumLabel(t, 'auditSurfaceLabels', event.surfaceKind)}</Badge>,
      displayText: (event) => enumLabel(t, 'auditSurfaceLabels', event.surfaceKind),
    },
    {
      id: 'result',
      label: t('admin.auditResult'),
      value: (event) => event.resultKind,
      cell: (event) => (
        <Badge tone={event.resultKind}>{enumLabel(t, 'auditResultLabels', event.resultKind)}</Badge>
      ),
      displayText: (event) => enumLabel(t, 'auditResultLabels', event.resultKind),
    },
    {
      id: 'created_at',
      label: t('admin.auditTime'),
      align: 'end',
      value: (event) => event.createdAt,
      cell: (event) => (
        <time dateTime={event.createdAt} title={event.createdAt}>
          {formatDate(event.createdAt, locale, TIMESTAMP_FORMAT)}
        </time>
      ),
      displayText: (event) => formatDate(event.createdAt, locale, TIMESTAMP_FORMAT),
    },
  ]
}

/**
 * The closed vocabularies this grid can show, as the owning `i18n` contract
 * enumerates them. A literal search resolves to the values whose *displayed*
 * label contains the term — never a handwritten language keyword dictionary.
 */
const AUDIT_LABEL_VOCABULARIES = [
  { column: 'action' as const, hint: 'actionKinds' as const, namespace: 'auditActionLabels' },
  { column: 'surface' as const, hint: 'surfaceKinds' as const, namespace: 'auditSurfaceLabels' },
  { column: 'result' as const, hint: 'resultKinds' as const, namespace: 'auditResultLabels' },
]

function i18nVocabulary(namespace: string, t: TFunction): string[] {
  const bundle = t(`admin.${namespace}`, { returnObjects: true, defaultValue: {} }) as unknown
  return bundle && typeof bundle === 'object' ? Object.keys(bundle) : []
}

/** The values of one closed vocabulary whose translated label contains the term. */
function matchingVocabularyValues(
  namespace: string,
  needle: string,
  locale: Locale,
  t: TFunction,
): string[] {
  return i18nVocabulary(namespace, t).filter((value) =>
    enumLabel(t, namespace, value).toLocaleLowerCase(locale).includes(needle),
  )
}

/**
 * The hints for one column's value search. A facet's own menu narrows only that
 * column, so the term is resolved against that column's formatter: a date
 * against the `Intl` interval, an enum against its label, the actor's null
 * bucket against the shared null token. The raw term is still sent by the
 * caller; this object only adds the typed, displayed-value spellings.
 */
export function deriveAuditColumnSearch(
  column: AuditGridColumnId,
  raw: string,
  locale: Locale,
  t: TFunction,
): AuditSearchHints {
  const search = raw.trim()
  if (!search) return {}
  const needle = search.toLocaleLowerCase(locale)
  if (column === 'created_at') {
    const range = parseLocalizedDateRange(search, locale)
    return range ? { createdAtRange: range } : {}
  }
  if (column === 'actor') {
    return auditNullLabel(t).toLocaleLowerCase(locale).includes(needle) ? { actorNull: true } : {}
  }
  const entry = AUDIT_LABEL_VOCABULARIES.find((candidate) => candidate.column === column)
  if (!entry) return {}
  const values = matchingVocabularyValues(entry.namespace, needle, locale, t)
  return values.length > 0 ? { [entry.hint]: values } : {}
}

/**
 * The one whole-table search derived once for the server: the raw term, the
 * closed enum values whose translated label contains it, the shared null token,
 * and an `Intl`-derived UTC interval when the term is a date/time the grid could
 * have rendered. Every label comes from the owning formatter and every separator
 * from `Intl`; the query's text scope is never widened by these hints.
 */
export function deriveAuditSearch(raw: string, locale: Locale, t: TFunction): AuditSearchHints {
  const search = raw.trim()
  if (!search) return {}
  const hints: AuditSearchHints = {}
  let any = false
  const vocabularies = [
    { column: 'action', hint: 'actionKinds' },
    { column: 'surface', hint: 'surfaceKinds' },
    { column: 'result', hint: 'resultKinds' },
  ] as const
  for (const { column, hint } of vocabularies) {
    const values = deriveAuditColumnSearch(column, search, locale, t)[hint]
    if (values && values.length > 0) {
      hints[hint] = values
      any = true
    }
  }
  const columnHints = deriveAuditColumnSearch('created_at', search, locale, t)
  if (columnHints.createdAtRange) {
    hints.createdAtRange = columnHints.createdAtRange
    any = true
  }
  if (deriveAuditColumnSearch('actor', search, locale, t).actorNull) {
    hints.actorNull = true
    any = true
  }
  return any ? hints : {}
}

/** Validate a typed filter key without throwing on a malformed URL value. */
function isValidFilterKey(key: string): boolean {
  try {
    gridFacetValue(key)
    return true
  } catch {
    return false
  }
}

/** Decode the `filters` URL parameter; unknown columns and malformed keys are dropped. */
export function parseAuditFilters(raw: string | null | undefined): GridState['filters'] {
  if (!raw) return {}
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    return {}
  }
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return {}
  const filters: GridState['filters'] = {}
  for (const [columnId, keys] of Object.entries(parsed as Record<string, unknown>)) {
    if (!GRID_COLUMN_IDS.has(columnId) || !Array.isArray(keys)) continue
    const valid = keys.filter(
      (key): key is string => typeof key === 'string' && isValidFilterKey(key),
    )
    if (valid.length > 0) filters[columnId] = valid
  }
  return filters
}

/** Encode the grid filters for the URL; null when nothing is selected. */
export function serializeAuditFilters(filters: GridState['filters']): string | null {
  const entries = Object.entries(filters).filter(([, keys]) => keys.length > 0)
  if (entries.length === 0) return null
  return JSON.stringify(Object.fromEntries(entries))
}

/** Decode the `sort` URL parameter into a grid sort, or null. */
export function parseAuditSort(raw: string | null | undefined): GridState['sort'] {
  if (!raw) return null
  const [columnId, direction] = raw.split(':')
  if (!columnId || (direction !== 'asc' && direction !== 'desc')) return null
  if (!GRID_COLUMN_IDS.has(columnId)) return null
  return { columnId, direction }
}

const COLUMN_FILTER_FIELD = {
  action: 'actionKind',
  surface: 'surfaceKind',
  result: 'resultKind',
  created_at: 'createdAt',
} as const satisfies Partial<Record<Exclude<AuditGridColumnId, 'actor'>, keyof AuditColumnFilters>>

type AuditColumnFilterField = (typeof COLUMN_FILTER_FIELD)[keyof typeof COLUMN_FILTER_FIELD]

/**
 * Decode the actor column's selected keys into the ids to match and the
 * "no actor" flag. A null key is a real flag, never an id.
 */
function actorColumnFilter(keys: readonly string[] | undefined): {
  actor?: string[]
  actorNull?: boolean
} {
  if (!keys || keys.length === 0) return {}
  const actor: string[] = []
  let actorNull = false
  for (const key of keys) {
    const value = gridFacetValue(key)
    if (value === null) actorNull = true
    else if (typeof value === 'string') actor.push(value)
  }
  const selected: { actor?: string[]; actorNull?: boolean } = {}
  if (actor.length > 0) selected.actor = actor
  if (actorNull) selected.actorNull = true
  return selected
}

/**
 * Decode one non-actor column's keys into its typed field and string values,
 * or `undefined` when the column is not part of the grid vocabulary or every
 * key is malformed for it.
 */
function columnFilterEntry(
  columnId: string,
  keys: readonly string[],
): { field: AuditColumnFilterField; values: string[] } | undefined {
  if (keys.length === 0) return undefined
  const field = COLUMN_FILTER_FIELD[columnId as Exclude<AuditGridColumnId, 'actor'>]
  if (!field) return undefined
  const values = keys
    .map(gridFacetValue)
    .filter((value): value is string => typeof value === 'string')
  return values.length > 0 ? { field, values } : undefined
}

/** Assign one decoded column's values onto its typed filter field. */
function assignColumnFilter(
  result: AuditColumnFilters,
  field: AuditColumnFilterField,
  values: string[],
): void {
  switch (field) {
    case 'actionKind':
      result.actionKind = values
      break
    case 'surfaceKind':
      result.surfaceKind = values
      break
    case 'resultKind':
      result.resultKind = values
      break
    case 'createdAt':
      result.createdAt = values
      break
  }
}

/**
 * Grid filter selection -> the typed per-column filters the list and facet
 * endpoints take. A null actor key becomes `actorNull`, never an id.
 */
export function auditColumnFilters(filters: GridState['filters']): AuditColumnFilters | undefined {
  const result: AuditColumnFilters = {}
  let any = false
  const actorFilter = actorColumnFilter(filters.actor)
  if (actorFilter.actor) {
    result.actor = actorFilter.actor
    any = true
  }
  if (actorFilter.actorNull) {
    result.actorNull = true
    any = true
  }
  for (const [columnId, keys] of Object.entries(filters)) {
    const entry = columnFilterEntry(columnId, keys)
    if (!entry) continue
    assignColumnFilter(result, entry.field, entry.values)
    any = true
  }
  return any ? result : undefined
}

export interface AuditGridColumnOptions {
  locale: Locale
  t: TFunction
  /** Show request/trace/internal detail in the action cell. */
  showInternal: boolean
  /** Show assistant metadata already authorized in the response. */
  showAssistant: boolean
}
