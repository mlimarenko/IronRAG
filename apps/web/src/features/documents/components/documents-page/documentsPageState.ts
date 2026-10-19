import { useCallback, useLayoutEffect, useMemo, useRef, useState } from 'react'
import type { Dispatch, SetStateAction } from 'react'

import type {
  AsyncOperationDetail,
  CatalogLibraryResponse,
  DocumentListSortKey,
  DocumentListSortOrder,
  DocumentIndexingFilter,
  DocumentListStatusFilter,
  WebIngestPattern,
} from '@/shared/api'
import type { WebIngestUrlFilter } from '@/shared/api/generated'
import { gridFacetValue, type GridState } from '@/shared/data-grid'
import { useQueuedSearchParams } from '@/shared/hooks/useQueuedSearchParams'
import {
  isStorageRecord,
  parseNumberOption,
  parseTableSort,
  useTableState,
  type TableSortState,
} from '@/shared/hooks/useTableState'
import {
  DOCUMENT_GRID_COLUMNS,
  documentsColumnFilters,
} from '@/features/documents/model/documentsGrid'
import { formatWebIngestPatterns } from '@/features/documents/model/webIngestPatterns'

export const PAGE_SIZE_OPTIONS = [25, 50, 100, 200] as const
export type PageSizeOption = (typeof PAGE_SIZE_OPTIONS)[number]
const DEFAULT_PAGE_SIZE: PageSizeOption = 50

export const SEARCH_DEBOUNCE_MS = 300
export const SELECTED_DETAIL_REFRESH_MS = 5000
export const LIST_POLL_GRACE_MS = 60_000
export const LIST_POLL_INTERVAL_MS = 2500

export type DocumentsPageTab = 'documents' | 'web'

/// `inflight` and `notready` are the two unions the rest of the product already
/// counts in: the dashboard sums `processing + queued` for its in-flight row and
/// everything but `ready` for its not-available card, and the API raises its
/// lagging-vector and lagging-relation notices from the same in-flight sum.
/// Those places used to link to a comma-joined list of statuses, which no
/// filter parsed — the reader landed on the unfiltered list showing every
/// document, which is not a broken page but is also not the set they clicked,
/// and it hid the one case worth seeing: a queue that stopped moving, where
/// every affected document sits in `queued` and none is `processing`.
///
/// `vectorgap`, `graphgap` and `graphready` are not statuses at all: those
/// documents are `ready` by every backend rule, and the complaint is that they
/// are offered as searchable while holding no chunk vectors, or while their
/// entities and relations were never extracted. They are the destinations of
/// the dashboard's indexing notices, and the server resolves each through its
/// own `indexing` parameter rather than a status value.
export type DocumentsStatusBucket =
  | 'all'
  | 'ready'
  | 'notready'
  | 'inflight'
  | 'vectorgap'
  | 'graphgap'
  | 'graphready'
  | 'processing'
  | 'queued'
  | 'failed'
  | 'canceled'
  | 'empty'

/// What each bucket asks the list endpoint for. Most narrow by derived status;
/// the indexing buckets narrow by an indexing invariant instead, which is why
/// this maps to a query fragment and not to a status array.
export type DocumentsListFilter = {
  status?: DocumentListStatusFilter[]
  indexing?: DocumentIndexingFilter
}

const BUCKET_TO_BACKEND: Record<Exclude<DocumentsStatusBucket, 'all'>, DocumentsListFilter> = {
  ready: { status: ['ready'] },
  notready: { status: ['processing', 'queued', 'failed', 'canceled', 'empty'] },
  inflight: { status: ['processing', 'queued'] },
  vectorgap: { indexing: 'vector_gap' },
  graphgap: { indexing: 'graph_gap' },
  graphready: { indexing: 'graph_ready' },
  processing: { status: ['processing'] },
  queued: { status: ['queued'] },
  failed: { status: ['failed'] },
  canceled: { status: ['canceled'] },
  empty: { status: ['empty'] },
}

const NO_LIST_FILTER: DocumentsListFilter = {}

/// Buckets that stand for a set of the primitive ones rather than a status a
/// document can be in. They are destinations a counted figure links to, not
/// filters the reader builds, so the filter row offers them only while one is
/// selected — see `DocumentList`. The indexing buckets belong here for the same
/// reason: they are reached from the dashboard, and none of them is a status.
export const UNION_STATUS_BUCKETS: readonly DocumentsStatusBucket[] = [
  'notready',
  'inflight',
  'vectorgap',
  'graphgap',
  'graphready',
]

/// The buckets resolved by an `indexing` invariant rather than a status. The
/// set of documents they select is `ready` by every lifecycle rule; what they
/// share is a missing (or complete) search index.
const INDEXING_FILTER_BUCKETS = ['vectorgap', 'graphgap', 'graphready'] as const
export type IndexingFilterBucket = (typeof INDEXING_FILTER_BUCKETS)[number]

export function isIndexingFilterBucket(
  bucket: DocumentsStatusBucket,
): bucket is IndexingFilterBucket {
  return (INDEXING_FILTER_BUCKETS as readonly string[]).includes(bucket)
}

export type SortValue = `${DocumentListSortKey}:${DocumentListSortOrder}`

const SORT_VALUES: readonly SortValue[] = [
  'uploaded_at:desc',
  'uploaded_at:asc',
  'file_name:asc',
  'file_name:desc',
  'file_type:asc',
  'file_type:desc',
  'file_size:asc',
  'file_size:desc',
  'status:asc',
  'status:desc',
  'cost:asc',
  'cost:desc',
  'pipeline_time:asc',
  'pipeline_time:desc',
  'finished_at:asc',
  'finished_at:desc',
]

const SORT_PARTS: Record<
  SortValue,
  { sortBy: DocumentListSortKey; sortOrder: DocumentListSortOrder }
> = {
  'uploaded_at:desc': { sortBy: 'uploaded_at', sortOrder: 'desc' },
  'uploaded_at:asc': { sortBy: 'uploaded_at', sortOrder: 'asc' },
  'file_name:asc': { sortBy: 'file_name', sortOrder: 'asc' },
  'file_name:desc': { sortBy: 'file_name', sortOrder: 'desc' },
  'file_type:asc': { sortBy: 'file_type', sortOrder: 'asc' },
  'file_type:desc': { sortBy: 'file_type', sortOrder: 'desc' },
  'file_size:asc': { sortBy: 'file_size', sortOrder: 'asc' },
  'file_size:desc': { sortBy: 'file_size', sortOrder: 'desc' },
  'status:asc': { sortBy: 'status', sortOrder: 'asc' },
  'status:desc': { sortBy: 'status', sortOrder: 'desc' },
  'cost:asc': { sortBy: 'cost', sortOrder: 'asc' },
  'cost:desc': { sortBy: 'cost', sortOrder: 'desc' },
  'pipeline_time:asc': { sortBy: 'pipeline_time', sortOrder: 'asc' },
  'pipeline_time:desc': { sortBy: 'pipeline_time', sortOrder: 'desc' },
  'finished_at:asc': { sortBy: 'finished_at', sortOrder: 'asc' },
  'finished_at:desc': { sortBy: 'finished_at', sortOrder: 'desc' },
}

type WebIngestFilterSnapshot = {
  allowPatterns: WebIngestPattern[]
  blockPatterns: WebIngestPattern[]
  allowText: string
  blockText: string
}

export type WebIngestPolicySnapshot = {
  crawlFilter: WebIngestFilterSnapshot
  materializationFilter: WebIngestFilterSnapshot
}

export type WebIngestPolicyDraft = {
  libraryId: string
  crawlAllowText: string
  crawlBlockText: string
  materializationAllowText: string
  materializationBlockText: string
}

export type UploadQueueItem = {
  id: string
  name: string
  state: 'uploading' | 'done' | 'error'
  error?: string | undefined
  errorAction?: string | undefined
  errorDiagnosticCode?: string | undefined
  errorDiagnosticMessage?: string | undefined
}

export type BulkRerunState = {
  kind: 'delete' | 'reprocess'
  operationId: string
  total: number
  completed: number
  failed: number
  inFlight: number
  status: AsyncOperationDetail['status']
}

export type DocumentsTableState = {
  pageSize: PageSizeOption
  sort: TableSortState<DocumentListSortKey>
  /**
   * Opt-in detail columns (Cost / Pipeline time / Finished). They are real
   * server columns now — sortable and filterable like the rest — but stay
   * collapsed by default to keep the table inside laptop widths until the
   * operator asks for them.
   */
  showDetailColumns: boolean
}

export type UpdateSearchParamState = (updates: Record<string, string | null>) => void

const DOCUMENTS_TABLE_ID = 'documents.list'
const DEFAULT_SORT_VALUE: SortValue = 'uploaded_at:desc'
const DEFAULT_DOCUMENTS_TABLE_STATE: DocumentsTableState = {
  pageSize: DEFAULT_PAGE_SIZE,
  sort: { key: 'uploaded_at', direction: 'desc' },
  showDetailColumns: false,
}

const DOCUMENT_SORT_KEYS: readonly DocumentListSortKey[] = [
  'uploaded_at',
  'file_name',
  'file_type',
  'file_size',
  'status',
  'cost',
  'pipeline_time',
  'finished_at',
]

const PAGE_SIZE_OPTION_VALUES = new Set<number>(PAGE_SIZE_OPTIONS)

function isPageSizeOption(value: number): value is PageSizeOption {
  return PAGE_SIZE_OPTION_VALUES.has(value)
}

function parsePageSize(
  value: string | null | undefined,
  fallback: PageSizeOption = DEFAULT_PAGE_SIZE,
): PageSizeOption {
  const parsed = Number.parseInt(value ?? '', 10)
  if (isPageSizeOption(parsed)) {
    return parsed
  }
  return fallback
}

function parseStatusBucket(value: string | null): DocumentsStatusBucket {
  if (
    value === 'ready' ||
    value === 'notready' ||
    value === 'inflight' ||
    value === 'vectorgap' ||
    value === 'graphgap' ||
    value === 'graphready' ||
    value === 'processing' ||
    value === 'queued' ||
    value === 'failed' ||
    value === 'canceled' ||
    value === 'empty'
  ) {
    return value
  }
  return 'all'
}

const SORT_VALUE_VALUES = new Set<string>(SORT_VALUES)

function parseSortValue(
  raw: string | null | undefined,
  fallback: SortValue = DEFAULT_SORT_VALUE,
): SortValue {
  return SORT_VALUE_VALUES.has(raw ?? '') ? (raw as SortValue) : fallback
}

export function splitSortValue(sort: SortValue): {
  sortBy: DocumentListSortKey
  sortOrder: DocumentListSortOrder
} {
  return SORT_PARTS[sort]
}

function sortPreferenceToSortValue(sort: TableSortState<DocumentListSortKey>): SortValue {
  if (!sort) return DEFAULT_SORT_VALUE
  return parseSortValue(`${sort.key}:${sort.direction}`)
}

function sortValueToPreference(sort: SortValue): TableSortState<DocumentListSortKey> {
  const parts = splitSortValue(sort)
  return { key: parts.sortBy, direction: parts.sortOrder }
}

function parseDocumentsTableState(raw: unknown): DocumentsTableState {
  const record = isStorageRecord(raw) ? raw : {}
  return {
    pageSize: parseNumberOption(
      record.pageSize,
      PAGE_SIZE_OPTIONS,
      DEFAULT_DOCUMENTS_TABLE_STATE.pageSize,
    ),
    sort: parseTableSort(record.sort, DOCUMENT_SORT_KEYS, DEFAULT_DOCUMENTS_TABLE_STATE.sort),
    showDetailColumns:
      typeof record.showDetailColumns === 'boolean'
        ? record.showDetailColumns
        : DEFAULT_DOCUMENTS_TABLE_STATE.showDetailColumns,
  }
}

export function useDocumentsTableState(): [
  DocumentsTableState,
  Dispatch<SetStateAction<DocumentsTableState>>,
] {
  return useTableState<DocumentsTableState>({
    tableId: DOCUMENTS_TABLE_ID,
    defaultValue: DEFAULT_DOCUMENTS_TABLE_STATE,
    parse: parseDocumentsTableState,
  })
}

function toWebIngestFilterSnapshot(
  filter: WebIngestUrlFilter | undefined,
): WebIngestFilterSnapshot {
  const allowPatterns = filter?.allowPatterns ?? []
  const blockPatterns = filter?.blockPatterns ?? []
  return {
    allowPatterns,
    blockPatterns,
    allowText: formatWebIngestPatterns(allowPatterns),
    blockText: formatWebIngestPatterns(blockPatterns),
  }
}

export function extractWebIngestPolicy(
  library: CatalogLibraryResponse | null | undefined,
): WebIngestPolicySnapshot {
  const policy = library?.webIngestPolicy
  return {
    crawlFilter: toWebIngestFilterSnapshot(policy?.crawlFilter),
    materializationFilter: toWebIngestFilterSnapshot(policy?.materializationFilter),
  }
}

/// What the empty list states. "No documents yet" is the true-empty-library
/// sentence; a search or a status filter that matched nothing is a different
/// statement, and an indexing bucket that matched nothing is a third.
const EMPTY_FILTER_MESSAGE = 'documents.noMatchingFilter'
const EMPTY_INDEXING_MESSAGE: Record<IndexingFilterBucket, string> = {
  vectorgap: 'documents.noVectorGaps',
  graphgap: 'documents.noGraphGaps',
  graphready: 'documents.noGraphReady',
}

export function documentsEmptyMessageKey({
  searchQuery,
  statusBucket,
}: {
  searchQuery: string
  statusBucket: DocumentsStatusBucket
}): string {
  if (searchQuery) return 'documents.noMatchingDocs'
  if (isIndexingFilterBucket(statusBucket)) return EMPTY_INDEXING_MESSAGE[statusBucket]
  if (statusBucket !== 'all') return EMPTY_FILTER_MESSAGE
  return 'documents.noDocs'
}

/// Whether the "select all matching" control may be offered. With a known
/// total it appears once the page is fully selected and more rows remain. With
/// an unknown total — the indexing buckets — the cursor is the only signal that
/// more matches exist, so it appears while a next page is still reachable.
export function selectAllMatchingAvailable({
  allVisibleSelected,
  filteredTotal,
  hasMorePages,
  itemCount,
  selectedCount,
}: {
  allVisibleSelected: boolean
  filteredTotal: number | null
  hasMorePages: boolean
  itemCount: number
  selectedCount: number
}): boolean {
  if (itemCount === 0 || !allVisibleSelected) return false
  if (filteredTotal == null) return hasMorePages
  return filteredTotal > itemCount && selectedCount < filteredTotal
}

export function parseCost(value: string | undefined | null): number | null {
  const parsed = Number.parseFloat(value ?? '')
  return Number.isNaN(parsed) ? null : parsed
}

export function getErrorMessage(error: unknown, fallback: string): string {
  return error instanceof Error && error.message ? error.message : fallback
}

const GRID_COLUMN_IDS = new Set<string>(DOCUMENT_GRID_COLUMNS)

/**
 * Only structurally valid typed keys enter UI state. Column-domain values are
 * validated by the API, which rejects an invalid value instead of broadening
 * the requested scope.
 */
function isValidFilterKey(key: string): boolean {
  try {
    gridFacetValue(key)
    return true
  } catch {
    return false
  }
}

/**
 * Decode the `filters` URL parameter. One JSON object per grid keeps a null
 * bucket, a value containing a comma and a decimal cost's every digit intact —
 * a comma-joined raw encoding would lose all three. Invalid JSON yields no
 * filters; unknown columns and malformed typed keys are ignored.
 */
export function parseGridFilters(raw: string | null | undefined): GridState['filters'] {
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
export function serializeGridFilters(filters: GridState['filters']): string | null {
  const entries = Object.entries(filters).filter(([, keys]) => keys.length > 0)
  if (entries.length === 0) return null
  return JSON.stringify(Object.fromEntries(entries))
}

/** Encode a server snapshot instant for the URL, or null. */
function serializeSnapshotAt(snapshotAt: string | null | undefined): string | null {
  return snapshotAt && snapshotAt.trim() !== '' ? snapshotAt : null
}

function gridStateFor({
  searchQuery,
  sortValue,
  filters,
  pageSize,
}: {
  searchQuery: string
  sortValue: SortValue
  filters: GridState['filters']
  pageSize: PageSizeOption
}): GridState {
  const parts = splitSortValue(sortValue)
  return {
    search: searchQuery,
    sort: { columnId: parts.sortBy, direction: parts.sortOrder },
    filters,
    // Cursor pagination has no page number; the grid keeps one for the local
    // mode it never uses here. Every scope change resets the cursor stack.
    page: 1,
    pageSize,
  }
}

export function useDocumentsPageUrlState({
  libraryId,
  tableState,
  setTableState,
}: {
  libraryId: string | null
  tableState: DocumentsTableState
  setTableState: Dispatch<SetStateAction<DocumentsTableState>>
}) {
  const [searchParams, setSearchParams] = useQueuedSearchParams()
  const [scope, setScope] = useState({
    libraryId,
    previousScopeSearch: null as string | null,
  })
  if (libraryId !== null && libraryId !== scope.libraryId) {
    setScope({
      libraryId,
      previousScopeSearch: scope.libraryId === null ? null : searchParams.toString(),
    })
  }
  const libraryChanged =
    (libraryId !== null && scope.libraryId !== null && libraryId !== scope.libraryId) ||
    scope.previousScopeSearch === searchParams.toString()
  const searchQuery = searchParams.get('q') ?? ''
  const sortValue = parseSortValue(
    searchParams.get('sort'),
    sortPreferenceToSortValue(tableState.sort),
  )
  const selectedDocumentId = libraryChanged ? null : searchParams.get('documentId')
  const statusBucket = parseStatusBucket(searchParams.get('status'))
  const pageSize = parsePageSize(searchParams.get('pageSize'), tableState.pageSize)
  const listFilter = useMemo(
    () => (statusBucket === 'all' ? NO_LIST_FILTER : BUCKET_TO_BACKEND[statusBucket]),
    [statusBucket],
  )
  const filters = useMemo(() => parseGridFilters(searchParams.get('filters')), [searchParams])
  const durationPinned = Boolean(filters.pipeline_time?.length)
  // Suppress the previous library clock before queries run in the new scope.
  // Initial context hydration is not a scope change: a reload retains its pin.
  const snapshotAt =
    libraryChanged && !durationPinned ? null : serializeSnapshotAt(searchParams.get('snapshotAt'))
  const currentScope = useRef({ libraryId, scope })
  useLayoutEffect(() => {
    currentScope.current = { libraryId, scope }
  }, [libraryId, scope])
  const updateSearchParamState = useCallback(
    (
      update:
        | Record<string, string | null>
        | ((params: URLSearchParams) => Record<string, string | null>),
    ) => {
      // An upload/selection callback from an unmounted library cannot rewrite
      // the URL owned by the newly selected scope.
      if (currentScope.current.libraryId !== libraryId || currentScope.current.scope !== scope)
        return
      setSearchParams((next) => {
        const updates = typeof update === 'function' ? update(next) : update
        if (Object.hasOwn(updates, 'pageSize')) {
          setTableState((prev) => ({
            ...prev,
            pageSize: parsePageSize(updates.pageSize, DEFAULT_PAGE_SIZE),
          }))
        }
        if (Object.hasOwn(updates, 'sort')) {
          setTableState((prev) => ({
            ...prev,
            sort: sortValueToPreference(parseSortValue(updates.sort, DEFAULT_SORT_VALUE)),
          }))
        }
        for (const [key, value] of Object.entries(updates)) {
          if (value == null || value === '') next.delete(key)
          else next.set(key, value)
        }
        return next
      })
    },
    [libraryId, scope, setSearchParams, setTableState],
  )

  useLayoutEffect(() => {
    if (libraryChanged) {
      updateSearchParamState({ documentId: null, ...(!durationPinned ? { snapshotAt: null } : {}) })
    }
  }, [durationPinned, libraryChanged, updateSearchParamState])

  const gridState = useMemo(
    () => gridStateFor({ searchQuery, sortValue, filters, pageSize }),
    [searchQuery, sortValue, filters, pageSize],
  )
  const columnFilters = useMemo(() => documentsColumnFilters(filters), [filters])
  const filtersKey = useMemo(() => serializeGridFilters(filters) ?? '', [filters])

  /**
   * Apply a grid transition. Search, sort, filter and page-size changes drop
   * the selected document and normally the duration snapshot pin. An active
   * duration-value filter keeps the clock it was selected against, otherwise a
   * processing row could disappear merely because the clock advanced. Reloads
   * also preserve the pin; an explicit refresh can advance it.
   */
  const applyGridState = useCallback(
    (update: SetStateAction<GridState>) => {
      updateSearchParamState((params) => {
        const next =
          typeof update === 'function'
            ? update(
                gridStateFor({
                  searchQuery: params.get('q') ?? '',
                  sortValue: parseSortValue(
                    params.get('sort'),
                    sortPreferenceToSortValue(tableState.sort),
                  ),
                  filters: parseGridFilters(params.get('filters')),
                  pageSize: parsePageSize(params.get('pageSize'), tableState.pageSize),
                }),
              )
            : update
        return {
          q: next.search,
          sort: next.sort ? `${next.sort.columnId}:${next.sort.direction}` : null,
          filters: serializeGridFilters(next.filters),
          pageSize: String(next.pageSize),
          documentId: null,
          snapshotAt: next.filters.pipeline_time?.length
            ? serializeSnapshotAt(params.get('snapshotAt'))
            : null,
        }
      })
    },
    [updateSearchParamState, tableState.pageSize, tableState.sort],
  )

  return {
    applyGridState,
    columnFilters,
    filters,
    filtersKey,
    gridState,
    listFilter,
    pageSize,
    searchQuery,
    selectedDocumentId,
    snapshotAt,
    sortValue,
    statusBucket,
    updateSearchParamState,
  }
}

/**
 * Is this library genuinely empty, as opposed to still loading, broken, or
 * merely filtered down to nothing?
 *
 * The empty state offers an upload button, so getting this wrong tells an
 * operator whose search matched nothing that their library has no documents at
 * all. Every clause is load-bearing: a failed first page must not read as
 * empty, a queued upload means content is on its way, and a status bucket or
 * search term narrowing the list to zero is a filter result, not emptiness.
 */
export function isLibraryEmpty(
  signals: Readonly<{
    loading: boolean
    loadFailed: boolean
    searchQuery: string
    statusBucket: DocumentsStatusBucket
    totalCount: number | null
    loadedCount: number
    pendingUploadCount: number
  }>,
): boolean {
  if (signals.loading || signals.loadFailed) return false
  if (signals.searchQuery || signals.statusBucket !== 'all') return false
  return signals.totalCount === 0 && signals.loadedCount === 0 && signals.pendingUploadCount === 0
}

/**
 * Does this screen have any spend to show? The cost strip is hidden entirely
 * when nothing has been charged and nothing went unpriced — an all-zero strip
 * reads as "costs are broken" rather than "nothing has cost anything".
 */
export function hasRecordedSpend(
  spend: Readonly<{
    libraryCost: number | null
    workspaceCost: number | null
    libraryUnpricedCalls: number | null
    workspaceUnpricedCalls: number | null
  }>,
): boolean {
  return (
    spend.libraryCost !== 0 ||
    spend.workspaceCost !== 0 ||
    spend.libraryUnpricedCalls !== 0 ||
    spend.workspaceUnpricedCalls !== 0
  )
}
