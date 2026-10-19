import { Content, Ops } from './generated'
import { infiniteQueryOptions } from '@tanstack/react-query'
import { client as generatedClient } from './generated/client.gen'
import { reportUnauthorizedResponse } from './sessionLifecycle'
import type {
  AsyncOperationDetailResponse,
  BatchCancelResponse,
  BatchDocumentOperationAcceptedResponse,
  ContentDocumentDetailResponse,
  ContentDocumentHead,
  ContentDocumentListItem,
  ContentMutationDetailResponse,
  ContentRevision,
  CreateDocumentResponse,
  CreateWebIngestRunRequest,
  DocumentColumnFilters as GeneratedDocumentColumnFilters,
  DocumentFacetColumn as GeneratedDocumentFacetColumn,
  DocumentFacetValue as GeneratedDocumentFacetValue,
  DocumentFacetsResponse as GeneratedDocumentFacetsResponse,
  DocumentListPageResponse as GeneratedDocumentListPageResponse,
  DocumentListSortKey as GeneratedDocumentListSortKey,
  DocumentListSortOrder as GeneratedDocumentListSortOrder,
  DocumentIndexingFilter as GeneratedDocumentIndexingFilter,
  DocumentStatus as GeneratedDocumentStatus,
  DocumentSearchRanges,
  FacetValue as GeneratedFacetValue,
  ListContentDocumentsData,
  ListContentDocumentFacetsData,
  ListContentPreparedSegmentsData,
  OverwriteMode,
  PreparedSegmentDetail,
  PreparedSegmentsPageResponse,
  SnapshotImportAcceptedResponse,
  TechnicalFactsPageResponse,
  TypedTechnicalFact,
  WebDiscoveredPage,
  WebIngestRunReceipt as GeneratedWebIngestRunReceipt,
  WebIngestRunSummary,
} from './generated'
import { ASYNC_OPERATION_TERMINAL_STATES } from './ops'
import { ApiError, type ApiErrorBody, unwrap } from './runtime'

export type DocumentListItem = ContentDocumentListItem
export type DocumentListPageResponse = GeneratedDocumentListPageResponse
export type DocumentListSortKey = GeneratedDocumentListSortKey
export type DocumentListSortOrder = GeneratedDocumentListSortOrder
export type DocumentColumnFilters = GeneratedDocumentColumnFilters
export type DocumentFacetColumn = GeneratedDocumentFacetColumn
export type DocumentFacetValue = GeneratedDocumentFacetValue
export type DocumentFacetsResponse = GeneratedDocumentFacetsResponse
export type DocumentFacetRawValue = GeneratedFacetValue

/**
 * Derived status buckets. Mirrors the backend `derived_status`
 * column in `list_document_page_rows`. The six values are the only ones
 * accepted by the `status` query parameter; anything else is rejected as
 * `400 Bad Request`.
 */
export type DocumentListStatusFilter = GeneratedDocumentStatus

export type DocumentIndexingFilter = GeneratedDocumentIndexingFilter

interface DocumentListParams {
  libraryId: string
  cursor?: string
  limit?: number
  search?: string
  sortBy?: DocumentListSortKey
  sortOrder?: DocumentListSortOrder
  includeDeleted?: boolean
  includeTotal?: boolean
  /** Empty / undefined = no filter. Sent as a comma-separated list. */
  status?: DocumentListStatusFilter[]
  /**
   * The grid's per-column filters. Serialized to JSON here, at the one owning
   * boundary, so no caller hand-writes the wire string. Null values are real
   * (the empty bucket) and decimals/dates travel as their exact source text.
   */
  columnFilters?: DocumentColumnFilters
  /**
   * Finite derived-status buckets a literal search matches, OR'd alongside the
   * text search over every canonical field. Sent comma-joined.
   */
  searchStatus?: DocumentListStatusFilter[]
  /**
   * Intl-normalized variant of `search`, OR'd over the same canonical fields.
   * The backend implements no language dictionary.
   */
  normalizedSearch?: string
  searchRanges?: DocumentSearchRanges
  /**
   * Narrow to documents a broken indexing invariant applies to. Orthogonal to
   * `status`: `vector_gap` documents are `ready`. Undefined = no filter, and a
   * filter matching nothing returns an empty page rather than the library.
   */
  indexing?: DocumentIndexingFilter
  /**
   * Restrict the page to specific document ids. Undefined omits the constraint;
   * an explicit empty set remains explicit and is rejected fail-closed.
   * Sent as a comma-separated list. Used to resolve a deep-linked
   * `documentId` through the shared list derivation so the inspector
   * opens even when the target is off the loaded page.
   */
  ids?: string[]
  /**
   * The snapshot instant this cursor chain was captured against. The server
   * pins its clock once per chain so a facet count and the list it opens see
   * the same moment; on a cursor page it must equal the cursor's snapshot.
   */
  snapshotAt?: string
}

export interface DocumentFacetParams {
  libraryId: string
  facetColumn: DocumentFacetColumn
  /**
   * The page's status pill, forwarded verbatim so the facet keeps the same
   * pill scope the list is showing while still self-excluding the grid's own
   * `status` column filter.
   */
  status?: DocumentListStatusFilter[]
  /** The same per-column filters the list carries; sent as JSON. */
  columnFilters?: DocumentColumnFilters
  search?: string
  searchStatus?: DocumentListStatusFilter[]
  normalizedSearch?: string
  searchRanges?: DocumentSearchRanges
  ids?: string[]
  indexing?: DocumentIndexingFilter
  includeDeleted?: boolean
  /**
   * The instant the list page was captured. Optional on the first page; on a
   * cursor page it must equal the cursor's snapshot exactly, so a facet count
   * and the page it opens agree.
   */
  snapshotAt?: string
  /** Server-side search across this column's values. */
  facetSearch?: string
  facetSearchRanges?: DocumentSearchRanges
  facetSearchStatus?: DocumentListStatusFilter[]
  facetCursor?: string
  facetLimit?: number
}

/** One serializer owns the search, filters and authorized scope for every reader. */
function documentQueryScope(params: DocumentFacetParams | Omit<DocumentListParams, 'libraryId'>) {
  const query: NonNullable<ListContentDocumentsData['query']> = {}
  if (params.includeDeleted !== undefined) query.includeDeleted = params.includeDeleted
  if (params.search !== undefined) query.search = params.search
  if (params.columnFilters) query.filters = JSON.stringify(params.columnFilters)
  if (params.searchStatus?.length) query.searchStatus = params.searchStatus.join(',')
  if (params.normalizedSearch !== undefined) query.normalizedSearch = params.normalizedSearch
  if (params.searchRanges) query.searchRanges = JSON.stringify(params.searchRanges)
  if (params.indexing !== undefined) query.indexing = params.indexing
  if (params.ids !== undefined) query.ids = params.ids.join(',')
  if (params.snapshotAt !== undefined) query.snapshotAt = params.snapshotAt
  if (params.status?.length) query.status = params.status.join(',')
  return query
}

export function documentListQuery(params: Omit<DocumentListParams, 'libraryId'>) {
  const query = documentQueryScope(params)
  if (params.cursor !== undefined) query.cursor = params.cursor
  if (params.limit !== undefined) query.limit = params.limit
  if (params.sortBy !== undefined) query.sortBy = params.sortBy
  if (params.sortOrder !== undefined) query.sortOrder = params.sortOrder
  if (params.includeTotal !== undefined) query.includeTotal = params.includeTotal
  return query
}

function documentFacetRequest(params: DocumentFacetParams) {
  const query: ListContentDocumentFacetsData['query'] = {
    ...documentQueryScope(params),
    facetColumn: params.facetColumn,
  }
  if (params.facetCursor !== undefined) query.facetCursor = params.facetCursor
  if (params.facetLimit !== undefined) query.facetLimit = params.facetLimit
  if (params.facetSearch !== undefined) query.facetSearch = params.facetSearch
  if (params.facetSearchRanges) query.facetSearchRanges = JSON.stringify(params.facetSearchRanges)
  if (params.facetSearchStatus?.length) query.facetSearchStatus = params.facetSearchStatus.join(',')
  return { path: { libraryId: params.libraryId }, query }
}

/** The scoped facet cache, cancellation and cursor chain belong to TanStack Query. */
export function documentFacetsInfiniteOptions(params: DocumentFacetParams) {
  const initialPageParam: { cursor: string | null; snapshotAt?: string } = { cursor: null }
  return infiniteQueryOptions({
    queryKey: ['documentFacets', params],
    initialPageParam,
    queryFn: ({ pageParam, signal }) =>
      documentsApi.facets(
        {
          ...params,
          ...(pageParam.cursor ? { facetCursor: pageParam.cursor } : {}),
          ...(pageParam.snapshotAt ? { snapshotAt: pageParam.snapshotAt } : {}),
        },
        signal,
      ),
    getNextPageParam: (last: DocumentFacetsResponse) =>
      last.nextCursor ? { cursor: last.nextCursor, snapshotAt: last.snapshotAt } : undefined,
  })
}

type BatchDeleteResponse = BatchDocumentOperationAcceptedResponse

/**
 * Standard 202 Accepted payload for `POST /content/documents/batch-reprocess`.
 *
 * The server schedules the actual per-document reruns on a background task
 * and returns the id of a **parent** `ops_async_operation` that the client
 * polls through the generated async-operation query to observe progress. All child
 * per-document mutations are linked back to this parent, so a single
 * indexed count query covers "completed / total / failed".
 */
type BatchReprocessAcceptedResponse = BatchDocumentOperationAcceptedResponse

type PreparedSegmentItem = PreparedSegmentDetail
export type WebIngestRunListItem = WebIngestRunSummary
export type WebIngestRunPageItem = WebDiscoveredPage
type WebIngestRunReceipt = GeneratedWebIngestRunReceipt

type DocumentUploadResponse = CreateDocumentResponse
type DocumentReprocessResponse = ContentMutationDetailResponse
type DocumentMutationResponse = ContentMutationDetailResponse

interface DocumentUploadOptions {
  documentHint?: string | undefined
  externalKey?: string | undefined
  fileName?: string | undefined
  title?: string | undefined
}

interface PreparedSegmentsPageParams {
  offset?: number
  limit?: number
}

const PREPARED_SEGMENTS_PAGE_LIMIT = 500

export const documentsApi = {
  /**
   * Identity keyset-paginated list. Callers drive infinite scroll by
   * threading `nextCursor` back into the next call; there is no
   * array-only shape. `includeTotal` is opt-in because the
   * backend executes a second unbounded `COUNT(*)` when it is set and
   * can be omitted when a caller only needs to walk the cursor chain.
   */
  list: (params: DocumentListParams): Promise<DocumentListPageResponse> => {
    const query = documentListQuery(params)

    return Content.listContentDocuments({ path: { libraryId: params.libraryId }, query }).then(
      (result): DocumentListPageResponse => unwrap(result),
    )
  },
  /**
   * Distinct values of one grid column, keyset-paginated on the canonical
   * value. The server self-excludes the column's own filter and applies every
   * other filter/search/scope predicate — including the page's status pill —
   * so a count and the page it opens cannot disagree; the client never computes
   * this column from one page.
   */
  facets: (params: DocumentFacetParams, signal?: AbortSignal): Promise<DocumentFacetsResponse> => {
    return Content.listContentDocumentFacets({
      ...documentFacetRequest(params),
      ...(signal ? { signal } : {}),
    }).then((result): DocumentFacetsResponse => unwrap(result))
  },
  get: (documentId: string) =>
    Content.getContentDocument({ path: { documentId } }).then(
      (result): ContentDocumentDetailResponse => unwrap(result),
    ),
  upload: (
    libraryId: string,
    file: File,
    options?: DocumentUploadOptions,
  ): Promise<DocumentUploadResponse> => {
    // openapi-ts currently exposes only the first request body when an
    // operation accepts both JSON and multipart. The generated client still
    // owns authentication and response handling; pass FormData directly to it
    // so the browser supplies the multipart boundary.
    const body = new FormData()
    const fileName = options?.fileName ?? file.name
    body.append('file', file, fileName)
    if (options?.documentHint) body.append('document_hint', options.documentHint)
    if (options?.externalKey) body.append('external_key', options.externalKey)
    if (options?.title) body.append('title', options.title)
    return generatedClient
      .post<{ 201: DocumentUploadResponse }, ApiErrorBody, true>({
        url: '/v1/content/libraries/{libraryId}/documents',
        path: { libraryId },
        body,
        // Send the FormData untouched: the default JSON serializer would
        // stringify it and the request would go out as text/plain, which the
        // API rejects with 415. A null serializer also lets the browser set
        // the multipart boundary itself.
        bodySerializer: null,
        headers: { 'Content-Type': null },
        throwOnError: true,
      })
      .then((result): DocumentUploadResponse => unwrap(result))
  },
  delete: (documentId: string) =>
    Content.deleteContentDocument({ path: { documentId } }).then((result) => {
      unwrap(result)
    }),
  updateDocumentHint: async (
    documentId: string,
    documentHint: string | null,
  ): Promise<string | null> => {
    const detail = await Content.patchContentDocumentMetadata({
      path: { documentId },
      body: { documentHint },
    }).then((result): ContentDocumentDetailResponse => unwrap(result))
    const savedDocumentHint = detail.activeRevision?.document_hint
    if (typeof savedDocumentHint !== 'string' && savedDocumentHint !== null) {
      throw new Error('document hint missing from saved response')
    }
    return savedDocumentHint
  },
  reprocess: (documentId: string) =>
    Content.reprocessContentDocument({
      path: { documentId },
      body: {},
    }).then((result): DocumentReprocessResponse => unwrap(result)),
  createWebIngestRun: (data: CreateWebIngestRunRequest) =>
    Content.createContentWebIngestRun({ body: data }).then((result): WebIngestRunReceipt =>
      unwrap(result),
    ),
  listWebRuns: async (libraryId: string, limit: number = 50): Promise<WebIngestRunListItem[]> => {
    return Content.listContentWebIngestRuns({
      query: { libraryId, limit },
    }).then((result): WebIngestRunListItem[] => unwrap(result))
  },
  listWebRunPages: async (runId: string): Promise<WebIngestRunPageItem[]> => {
    return Content.listContentWebIngestRunPages({
      path: { runId },
    }).then((result): WebIngestRunPageItem[] => unwrap(result))
  },
  cancelWebRun: (runId: string) =>
    Content.cancelContentWebIngestRun({ path: { runId } }).then((result): WebIngestRunReceipt =>
      unwrap(result),
    ),
  edit: (documentId: string, markdown: string) =>
    Content.createContentRevision({
      path: { documentId },
      body: { mode: 'replace', markdown },
    }).then((result): DocumentMutationResponse => unwrap(result)),
  replace: (documentId: string, file: File): Promise<DocumentMutationResponse> => {
    const body = new FormData()
    body.append('file', file, file.name)

    return generatedClient
      .post<{ 202: DocumentMutationResponse }, ApiErrorBody, true>({
        url: '/v1/content/documents/{documentId}/revisions',
        path: { documentId },
        body,
        // Send the FormData untouched: the default JSON serializer would
        // stringify it and the request would go out as text/plain, which the
        // API rejects with 415. A null serializer also lets the browser set
        // the multipart boundary itself.
        bodySerializer: null,
        headers: { 'Content-Type': null },
        throwOnError: true,
      })
      .then((result): DocumentMutationResponse => unwrap(result))
  },
  getHead: (documentId: string) =>
    Content.getContentDocumentHead({ path: { documentId } }).then((result): ContentDocumentHead =>
      unwrap(result),
    ),
  getPreparedSegmentsPage: (
    documentId: string,
    params: PreparedSegmentsPageParams = {},
  ): Promise<PreparedSegmentsPageResponse> => {
    const query: NonNullable<ListContentPreparedSegmentsData['query']> = {}
    if (params.offset !== undefined) query.offset = params.offset
    if (params.limit !== undefined) query.limit = params.limit

    return Content.listContentPreparedSegments({
      path: { documentId },
      query,
    }).then((result): PreparedSegmentsPageResponse => unwrap(result))
  },
  getAllPreparedSegments: async (documentId: string) => {
    const segments: PreparedSegmentItem[] = []
    let offset = 0

    while (true) {
      const response = await documentsApi.getPreparedSegmentsPage(documentId, {
        offset,
        limit: PREPARED_SEGMENTS_PAGE_LIMIT,
      })
      const pageItems = response.items ?? []
      segments.push(...pageItems)

      const total = typeof response.total === 'number' ? response.total : null
      if (pageItems.length === 0) {
        break
      }
      if (total != null && segments.length >= total) {
        break
      }
      if (total == null && pageItems.length < PREPARED_SEGMENTS_PAGE_LIMIT) {
        break
      }

      offset += pageItems.length
    }

    return segments
  },
  getTechnicalFacts: async (documentId: string): Promise<TypedTechnicalFact[]> => {
    const response = await Content.listContentTechnicalFacts({
      path: { documentId },
    }).then((result): TechnicalFactsPageResponse => unwrap(result))
    return response.items ?? []
  },
  getSourceBlob: (documentId: string, revisionId?: string): Promise<Blob> =>
    Content.getContentDocumentSource({
      path: { documentId },
      ...(revisionId ? { query: { revisionId } } : {}),
      // Every download revalidates visibility on the server. Text sources also
      // need explicit Blob parsing; the generated client's MIME inference uses text.
      cache: 'no-store',
      parseAs: 'blob',
    }).then((result): Blob => unwrap(result)),
  getSourceText: async (sourceHref: string) => {
    const response = await fetch(sourceHref, { credentials: 'include', cache: 'no-store' })
    if (!response.ok) {
      // A document's source link is this API for stored uploads but the
      // origin site's own address for web-ingested pages. Only our own 401
      // says anything about our session; someone else's must not sign the
      // user out.
      if (isApplicationOrigin(sourceHref)) reportUnauthorizedResponse(response.status)
      throw new ApiError(response.status, await readApiErrorBody(response))
    }
    return response.text()
  },
  getEditorSourceText: async (documentId: string) => {
    const query = new URLSearchParams({ representation: 'editor_markdown' })
    const response = await fetch(
      `/v1/content/documents/${encodeURIComponent(documentId)}/source?${query}`,
      { credentials: 'include', cache: 'no-store' },
    )
    if (!response.ok) {
      reportUnauthorizedResponse(response.status)
      throw new ApiError(response.status, await readApiErrorBody(response))
    }
    return response.text()
  },
  getRevisions: (documentId: string) =>
    Content.listContentRevisions({ path: { documentId } }).then((result): ContentRevision[] =>
      unwrap(result),
    ),
  batchDelete: (documentIds: string[]) =>
    Content.batchDeleteContentDocuments({
      body: { documentIds },
    }).then((result): BatchDeleteResponse => unwrap(result)),
  batchCancel: (documentIds: string[]) =>
    Content.batchCancelContentDocuments({
      body: { documentIds },
    }).then((result): BatchCancelResponse => unwrap(result)),
  batchReprocess: (documentIds: string[]) =>
    Content.batchReprocessContentDocuments({
      body: { documentIds },
    }).then((result): BatchReprocessAcceptedResponse => unwrap(result)),
}

export type LibrarySnapshotIncludeKind = 'library_data' | 'blobs' | 'workspace' | 'ai_config'

export type LibrarySnapshotOverwriteMode = OverwriteMode

type LibrarySnapshotImportAccepted = SnapshotImportAcceptedResponse

const SNAPSHOT_IMPORT_POLL_INTERVAL_MS = 2_000
const SNAPSHOT_IMPORT_TIMEOUT_MS = 30 * 60 * 1_000

export class SnapshotImportTimeoutError extends Error {
  constructor() {
    super('snapshot import did not reach a terminal state before the deadline')
    this.name = 'SnapshotImportTimeoutError'
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

function snapshotApiBaseUrl(): string {
  const configured = generatedClient.getConfig().baseUrl
  if (configured) return configured.replace(/\/$/, '')
  if (globalThis.location !== undefined && globalThis.location.origin !== 'null') {
    return globalThis.location.origin
  }
  return ''
}

/** Does this href address this application, rather than some other site? */
function isApplicationOrigin(href: string): boolean {
  try {
    return new URL(href, window.location.origin).origin === window.location.origin
  } catch {
    return false
  }
}

async function readApiErrorBody(response: Response): Promise<ApiErrorBody> {
  const text = await response.text()
  if (!text) return { status: response.status }
  try {
    const parsed = JSON.parse(text) as Record<string, unknown>
    return {
      ...(typeof parsed.detail === 'string' ? { detail: parsed.detail } : {}),
      ...(typeof parsed.code === 'string' ? { code: parsed.code } : {}),
      ...(typeof parsed.title === 'string' ? { title: parsed.title } : {}),
      status: response.status,
    }
  } catch {
    return { detail: text, status: response.status }
  }
}

function isSnapshotImportAccepted(value: unknown): value is LibrarySnapshotImportAccepted {
  if (typeof value !== 'object' || value === null) return false
  const payload = value as Record<string, unknown>
  return (
    typeof payload.archiveBytes === 'number' &&
    Number.isSafeInteger(payload.archiveBytes) &&
    payload.archiveBytes >= 0 &&
    typeof payload.libraryId === 'string' &&
    typeof payload.operationId === 'string' &&
    (payload.overwriteMode === 'reject' || payload.overwriteMode === 'replace') &&
    typeof payload.workspaceId === 'string'
  )
}

/**
 * Library backup/restore API — tar.zst archive with optional family
 * selection. Export is triggered via plain navigation (no fetch, no Blob
 * buffering) so the browser streams the response straight to disk —
 * multi-GB libraries download cleanly without memory pressure. Import
 * uses `fetch` with a streaming body source (File); the body is the raw
 * archive, not multipart.
 */
export const librarySnapshotApi = {
  /**
   * Builds the direct export URL. Navigate to this URL (via
   * `<a href download>` or `window.location`) to trigger a browser
   * download directly from the response body — no `fetch` wrapper.
   */
  exportUrl: (libraryId: string, include: LibrarySnapshotIncludeKind[]): string => {
    const qs = new URLSearchParams()
    if (include.length > 0) qs.set('include', include.join(','))
    const query = qs.toString()
    const suffix = query ? `?${query}` : ''
    return `/v1/content/libraries/${libraryId}/snapshot${suffix}`
  },
  /**
   * Triggers a browser download of the export URL. Creates an anchor
   * element, clicks it, and removes it — the browser handles the
   * streaming. No JavaScript memory buffer is allocated for the
   * archive body.
   */
  downloadExport: (libraryId: string, include: LibrarySnapshotIncludeKind[]): void => {
    const url = librarySnapshotApi.exportUrl(libraryId, include)
    const anchor = document.createElement('a')
    anchor.href = url
    anchor.rel = 'noopener'
    document.body.appendChild(anchor)
    anchor.click()
    anchor.remove()
  },
  /**
   * Restores a library from a tar.zst archive. The include kinds are
   * read from the manifest inside the archive; the client only picks
   * the overwrite mode.
   */
  import: (
    libraryId: string,
    file: File,
    overwrite: LibrarySnapshotOverwriteMode,
  ): Promise<LibrarySnapshotImportAccepted> => {
    const query = overwrite !== 'reject' ? `?overwrite=${encodeURIComponent(overwrite)}` : ''
    return fetch(
      `${snapshotApiBaseUrl()}/v1/content/libraries/${encodeURIComponent(libraryId)}/snapshot${query}`,
      {
        body: file,
        credentials: 'include',
        headers: { 'Content-Type': 'application/zstd' },
        method: 'POST',
      },
    ).then(async (response): Promise<LibrarySnapshotImportAccepted> => {
      if (!response.ok) {
        throw new ApiError(response.status, await readApiErrorBody(response))
      }
      if (response.status !== 202) {
        throw new Error(`snapshot import returned unexpected HTTP ${response.status}`)
      }
      const payload: unknown = await response.json()
      if (!isSnapshotImportAccepted(payload)) {
        throw new Error('snapshot import returned an invalid accepted response')
      }
      return payload
    })
  },
  waitForImport: async (
    operationId: string,
    timeoutMs = SNAPSHOT_IMPORT_TIMEOUT_MS,
  ): Promise<AsyncOperationDetailResponse> => {
    const deadline = Date.now() + timeoutMs
    while (true) {
      const operation = await Ops.getAsyncOperation({
        path: { operationId },
      }).then((result): AsyncOperationDetailResponse => unwrap(result))
      if (ASYNC_OPERATION_TERMINAL_STATES.has(operation.status)) {
        return operation
      }
      if (Date.now() >= deadline) {
        throw new SnapshotImportTimeoutError()
      }
      await sleep(SNAPSHOT_IMPORT_POLL_INTERVAL_MS)
    }
  },
}
