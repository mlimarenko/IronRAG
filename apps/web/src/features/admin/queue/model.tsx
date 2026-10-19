import type { TFunction } from 'i18next'

import type {
  IngestAttempt,
  IngestAttemptResponse,
  IngestJobResponse,
  IngestQueueItemResponse,
  IngestQueueResponse,
  IngestStageEvent,
  IngestStageTimelineResponse,
} from '@/shared/api/generated'
import {
  gridFacetKey,
  gridFacetValue,
  type GridColumn,
  type GridState,
  type GridValue,
} from '@/shared/data-grid'
import { humanizeDocumentFailure, humanizeDocumentStage } from '@/shared/lib/document-processing'
import { humanizeToken, queueStateLabel } from '@/shared/lib/enumLabels'
import { Badge, Progress } from '@/shared/ui'
import { screenPhase } from '@/shared/lib/screenPhase'

export type QueueItem = Readonly<IngestQueueItemResponse>

export type QueuePage = Readonly<{
  items: QueueItem[]
  summary: Readonly<{
    paused: number
    queued: number
    running: number
    total: number
  }>
}>

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value && typeof value === 'object' && !Array.isArray(value))
}

function isInteger(value: unknown): value is number {
  return typeof value === 'number' && Number.isInteger(value)
}

/**
 * A queue response that violates the wire contract the backend owns. `detail`
 * names the offending field for diagnostics; the Queue view translates it to a
 * localized fallback so a raw field diagnostic is never the user's message.
 */
export class QueueResponseError extends Error {
  constructor(public readonly detail: string) {
    super(detail)
    this.name = 'QueueResponseError'
  }
}

/**
 * The item fields the contract declares required and non-null. The renderer
 * reads them unconditionally — as a row key, a label or a humanized token — so
 * a null or missing one is refused, never coerced.
 */
const QUEUE_ITEM_REQUIRED_STRING_FIELDS = [
  'jobId',
  'workspaceId',
  'workspaceName',
  'libraryId',
  'libraryName',
  'documentName',
  'jobKind',
  'queueState',
  'queuedAt',
  'availableAt',
] as const

const QUEUE_ITEM_REQUIRED_BOOLEAN_FIELDS = [
  'canCancel',
  'canPause',
  'canResume',
  'canRetryRequeue',
  'hasStaleQueueLease',
] as const

/**
 * Nullable strings the contract declares. A null or absent value is valid;
 * any other type is not — `currentStage` and `failureCode` reach `humanize*`,
 * which would crash on a non-string.
 */
const QUEUE_ITEM_NULLABLE_STRING_FIELDS = [
  'attemptId',
  'attemptState',
  'currentStage',
  'documentId',
  'failureCode',
  'failureMessage',
  'heartbeatAt',
  'startedAt',
] as const

/** Nullable integer fields the contract declares (`int32`/`int64`). */
const QUEUE_ITEM_NULLABLE_NUMBER_FIELDS = [
  'attemptNumber',
  'progressPercent',
  'queuePosition',
] as const

const QUEUE_SUMMARY_FIELDS = ['paused', 'queued', 'running', 'total'] as const

function assertQueueItem(item: unknown, index: number): asserts item is QueueItem {
  if (!isRecord(item)) {
    throw new QueueResponseError(`Ingest queue item ${index} must be an object`)
  }
  for (const field of QUEUE_ITEM_REQUIRED_STRING_FIELDS) {
    if (typeof item[field] !== 'string') {
      throw new QueueResponseError(`Ingest queue item ${index} ${field} must be a string`)
    }
  }
  for (const field of QUEUE_ITEM_REQUIRED_BOOLEAN_FIELDS) {
    if (typeof item[field] !== 'boolean') {
      throw new QueueResponseError(`Ingest queue item ${index} ${field} must be a boolean`)
    }
  }
  for (const field of QUEUE_ITEM_NULLABLE_STRING_FIELDS) {
    const value = item[field]
    if (value !== null && value !== undefined && typeof value !== 'string') {
      throw new QueueResponseError(`Ingest queue item ${index} ${field} must be a string or null`)
    }
  }
  for (const field of QUEUE_ITEM_NULLABLE_NUMBER_FIELDS) {
    const value = item[field]
    if (value !== null && value !== undefined && !isInteger(value)) {
      throw new QueueResponseError(`Ingest queue item ${index} ${field} must be an integer or null`)
    }
  }
}

/**
 * Validate the queue rows at the API boundary. The generated client types are
 * compile-time only, so a malformed response would otherwise reach the grid: a
 * null `queueState` crashes the status label, a non-string `currentStage` or
 * `failureCode` crashes `humanize*`, a fractional or non-numeric field breaks
 * the declared integer wire type, and a
 * repeated `jobId` collides React keys into duplicate rows on every sort.
 * Refuse the response with a controlled error instead of coercing it.
 */
function assertQueueItems(items: unknown): asserts items is QueueItem[] {
  if (!Array.isArray(items)) {
    throw new QueueResponseError('Ingest queue response items must be an array')
  }
  const seen = new Set<string>()
  items.forEach((item, index) => {
    assertQueueItem(item, index)
    if (seen.has(item.jobId)) {
      throw new QueueResponseError(`Ingest queue response repeats jobId ${item.jobId}`)
    }
    seen.add(item.jobId)
  })
}

function assertQueueSummary(summary: unknown): asserts summary is QueuePage['summary'] {
  if (!isRecord(summary)) {
    throw new QueueResponseError('Ingest queue response summary must be an object')
  }
  for (const field of QUEUE_SUMMARY_FIELDS) {
    if (!isInteger(summary[field])) {
      throw new QueueResponseError(`Ingest queue response summary ${field} must be an integer`)
    }
  }
}

export function mapQueue(raw: IngestQueueResponse): QueuePage {
  // The response is validated once, at the boundary, before any reader trusts a
  // field. The backend always serializes both `items` and `summary` (Rust
  // non-`Option` struct fields, OpenAPI `required`), so a missing, null or
  // non-object envelope is a contract violation — a controlled query error,
  // never a fabricated empty page.
  if (!isRecord(raw)) {
    throw new QueueResponseError('Ingest queue response must be an object')
  }
  const { items, summary } = raw
  assertQueueItems(items)
  assertQueueSummary(summary)
  return { items, summary }
}

export function mapJob(raw: IngestJobResponse): IngestJobResponse {
  return raw
}

export function mapAttempts(raw: { items: IngestAttempt[] }): IngestAttempt[] {
  return raw.items
}

export function mapAttempt(raw: IngestAttemptResponse): IngestAttemptResponse {
  return raw
}

export function mapStageEvents(raw: IngestStageTimelineResponse): IngestStageEvent[] {
  return raw.stages.toSorted((first, second) => first.ordinal - second.ordinal)
}

const QUEUE_STATE_FILTERS = ['all', 'failed', 'paused', 'queued', 'running'] as const
export type QueueStateFilter = (typeof QUEUE_STATE_FILTERS)[number]

/**
 * The queue states each named bucket covers. A job leased to a worker but not
 * yet reporting a stage is `leased`; an operator filtering for running work
 * still expects to see it, so the bucket names both.
 */
const QUEUE_BUCKET_STATES: Record<Exclude<QueueStateFilter, 'all'>, readonly string[]> = {
  failed: ['failed'],
  paused: ['paused'],
  queued: ['queued'],
  running: ['leased', 'running'],
}

/** The `queue_state` facet keys a named bucket selects, empty for `all`. */
export function queueBucketKeys(bucket: QueueStateFilter): string[] {
  if (bucket === 'all') return []
  return QUEUE_BUCKET_STATES[bucket].map((state) => gridFacetKey(state))
}

/**
 * The named bucket a `queue_state` selection equals, or `all`. A custom
 * multi-value selection that is not exactly one bucket reads as `all`: it is
 * not a named scope and the URL carries it in `filters` instead.
 */
export function queueBucketFor(keys: readonly string[] | undefined): QueueStateFilter {
  if (!keys || keys.length === 0) return 'all'
  const values = new Set(keys.map((key) => gridFacetValue(key)))
  for (const bucket of QUEUE_STATE_FILTERS) {
    if (bucket === 'all') continue
    const states = QUEUE_BUCKET_STATES[bucket]
    if (values.size === states.length && states.every((state) => values.has(state))) return bucket
  }
  return 'all'
}

/** Change only the status bucket, preserving the library and other URL state. */
export function queueParamsWithState(params: URLSearchParams, state: QueueStateFilter) {
  const next = new URLSearchParams(params)
  if (state === 'all') next.delete('status')
  else next.set('status', state)
  return next
}

/** URL-pinned library scope must never intersect an unrelated shell workspace. */
export function resolveQueueRoute(
  params: URLSearchParams,
  shellLibraryId: string | undefined,
  shellWorkspaceId: string | undefined,
) {
  const explicitLibraryId = params.get('libraryId')?.trim() ?? null
  const requestedStatus = params.get('status')
  return {
    explicitLibraryId,
    scope: {
      libraryId: explicitLibraryId ?? shellLibraryId,
      workspaceId: explicitLibraryId !== null ? undefined : shellWorkspaceId,
    },
    stateFilter: QUEUE_STATE_FILTERS.find((filter) => filter === requestedStatus) ?? 'all',
  }
}

/** An explicit URL scope needs a valid library; a shell title is optional. */
export function queueScreenPhase(
  explicitLibraryId: string | null,
  queue: { isPending: boolean; error: unknown },
  library: { isPending: boolean; error: unknown },
) {
  if (explicitLibraryId === '') return 'error'
  const requireLibrary = explicitLibraryId !== null
  return screenPhase({
    loading: queue.isPending || (requireLibrary && library.isPending),
    failed: Boolean(queue.error) || (requireLibrary && Boolean(library.error)),
  })
}

/**
 * The online data columns of the queue table. Ids are the row's own field
 * names (`queuePosition`, `documentName`, `libraryName`, `currentStage`,
 * `queueState`, `progressPercent`, `attemptNumber`) so a column id is at once
 * the sort/filter key, the facet column and the row field a reader points at.
 */
const QUEUE_GRID_COLUMNS = [
  'queue_position',
  'document_name',
  'library_name',
  'current_stage',
  'queue_state',
  'progress_percent',
  'attempt_number',
] as const

const GRID_COLUMN_IDS = new Set<string>(QUEUE_GRID_COLUMNS)

function isFilterKey(key: string): boolean {
  try {
    gridFacetValue(key)
    return true
  } catch {
    return false
  }
}

/**
 * Decode the `filters` URL parameter. One JSON object per grid keeps a null
 * bucket and a value containing a separator intact; a comma-joined raw
 * encoding would lose both. Invalid JSON yields no filters; unknown columns and
 * malformed typed keys are ignored.
 */
export function parseQueueFilters(raw: string | null | undefined): GridState['filters'] {
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
    const valid = keys.filter((key): key is string => typeof key === 'string' && isFilterKey(key))
    if (valid.length > 0) filters[columnId] = valid
  }
  return filters
}

/** Encode the grid filters for the URL; null when nothing is selected. */
export function serializeQueueFilters(filters: GridState['filters']): string | null {
  const entries = Object.entries(filters).filter(([, keys]) => keys.length > 0)
  if (entries.length === 0) return null
  return JSON.stringify(Object.fromEntries(entries))
}

/**
 * The `queue_state` portion of the URL. It is stored under the named bucket
 * alias `status` when the selection equals one (`?status=running`), and under
 * the generic `filters` object otherwise. The two are never read together: the
 * bucket wins only when no explicit `filters.queue_state` is present.
 */
export function queueFiltersFromUrl(
  status: QueueStateFilter,
  filtersParam: string | null | undefined,
): GridState['filters'] {
  const filters = parseQueueFilters(filtersParam)
  if (!filters.queue_state && status !== 'all') {
    filters.queue_state = queueBucketKeys(status)
  }
  return filters
}

/** Decode the `sort` URL parameter into a grid sort, or null. */
export function parseQueueSort(raw: string | null | undefined): GridState['sort'] {
  if (!raw) return null
  const [columnId, direction] = raw.split(':')
  if (!columnId || (direction !== 'asc' && direction !== 'desc')) return null
  if (!GRID_COLUMN_IDS.has(columnId)) return null
  return { columnId, direction }
}

export interface QueueGridColumnOptions {
  t: TFunction
  /** Open the inspector for a row; wired to the document-name cell's button. */
  onOpen: (item: QueueItem) => void
}

function queueFailureLabel(item: QueueItem, t: TFunction): string | null {
  if (item.failureCode && !(item.hasStaleQueueLease && item.failureCode === 'stale_heartbeat')) {
    return (
      humanizeDocumentFailure({ failureCode: item.failureCode, stage: item.currentStage }, t) ??
      humanizeToken(item.failureCode)
    )
  }
  return null
}

function progressText(value: GridValue): string {
  return typeof value === 'number' ? `${value}%` : ''
}

/**
 * The queue grid columns. Every column states the same label in its cell, its
 * facet menu and its search index, so a reader who sees "Running" can search
 * for it and find the matching facet value. The raw value is searchable too,
 * through the grid's default `displayText` + `value` index.
 */
export function queueGridColumns({ t, onOpen }: QueueGridColumnOptions): GridColumn<QueueItem>[] {
  return [
    {
      id: 'queue_position',
      label: t('admin.queuePosition'),
      align: 'end',
      value: (item) => item.queuePosition ?? null,
      cell: (item) => item.queuePosition ?? '—',
      displayText: (item) => (item.queuePosition == null ? '' : String(item.queuePosition)),
    },
    {
      id: 'document_name',
      label: t('documents.name'),
      value: (item) => item.documentName,
      cell: (item) => (
        <button
          type="button"
          className="table-link queue-row-open"
          // The row toggles selection; opening the inspector is a distinct
          // action and must not also select.
          onClick={(event) => {
            event.stopPropagation()
            onOpen(item)
          }}
        >
          <strong>{item.documentName}</strong>
          <small>{humanizeToken(item.jobKind)}</small>
        </button>
      ),
      searchText: (item) => `${item.documentName} ${humanizeToken(item.jobKind)}`,
    },
    {
      id: 'library_name',
      label: t('common.library'),
      value: (item) => item.libraryName,
      cell: (item) => (
        <div className="cell-title">
          <span>{item.libraryName}</span>
          <small>{item.workspaceName}</small>
        </div>
      ),
      searchText: (item) => `${item.libraryName} ${item.workspaceName}`,
    },
    {
      id: 'current_stage',
      label: t('admin.queueStage'),
      value: (item) => item.currentStage ?? null,
      // The filter key stays the raw stage token; the order follows the
      // humanized label the cell and the menu show, so a locale whose label
      // order differs from the raw token order still reads alphabetically.
      sortValue: (item) => humanizeDocumentStage(item.currentStage, t) ?? '',
      cell: (item) => humanizeDocumentStage(item.currentStage, t) ?? '—',
      displayText: (item) => humanizeDocumentStage(item.currentStage, t) ?? '',
    },
    {
      id: 'queue_state',
      label: t('common.status'),
      value: (item) => item.queueState,
      cell: (item) => {
        const failure = queueFailureLabel(item, t)
        return (
          <div className="cell-title">
            <Badge tone={item.queueState}>{queueStateLabel(item.queueState, t)}</Badge>
            {item.hasStaleQueueLease ? <Badge tone="warning">{t('admin.staleLease')}</Badge> : null}
            {failure ? (
              <small className="failure-text" title={item.failureCode ?? ''}>
                {failure}
              </small>
            ) : null}
          </div>
        )
      },
      displayText: (item) => queueStateLabel(item.queueState, t),
      searchText: (item) =>
        [queueStateLabel(item.queueState, t), item.queueState, queueFailureLabel(item, t) ?? '']
          .filter(Boolean)
          .join(' '),
    },
    {
      id: 'progress_percent',
      label: t('admin.queueInspectorProgress'),
      align: 'end',
      value: (item) => item.progressPercent ?? null,
      cell: (item) =>
        typeof item.progressPercent === 'number' ? (
          <div className="queue-progress">
            <span>{item.progressPercent}%</span>
            <Progress value={item.progressPercent} label={t('admin.queueInspectorProgress')} />
          </div>
        ) : (
          '—'
        ),
      displayText: (item) => progressText(item.progressPercent ?? null),
    },
    {
      id: 'attempt_number',
      label: t('admin.attempt'),
      align: 'end',
      value: (item) => item.attemptNumber ?? null,
      cell: (item) => item.attemptNumber ?? '—',
      displayText: (item) => (item.attemptNumber == null ? '' : String(item.attemptNumber)),
    },
  ]
}
