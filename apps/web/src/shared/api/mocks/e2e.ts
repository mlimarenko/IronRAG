import { http, HttpResponse, type HttpHandler } from 'msw'
import type { Branding } from '@/app/branding'

import type {
  AdminLibraryPageResponse,
  AuditEventResponse,
  AuditSearchHintsInput,
  AdminLibraryFacetValue,
  AdminLibrarySearchHints,
  CatalogLibraryResponse,
  IngestQueueItemResponse,
  AiAccountResponse,
  AiBindingResponse,
  AssistantSessionListItem,
  AssistantHydratedConversation,
  AssistantSystemPromptResponse,
  ContentDocumentDetailResponse,
  ContentDocumentListItem,
  ContentRevision,
  DocumentReadiness,
  DocumentStatus,
  DocumentListPageResponse,
  DeploymentReadinessSnapshot,
  GetLibraryDashboardResponse,
  LibraryCostSummary,
  ListContentWebIngestRunsResponse,
  ListQuerySessionsResponse,
  GetIamSessionResponse,
  ModelCatalogEntryResponse,
  McpCapabilitySnapshot,
  ProviderCatalogEntryResponse,
  ShellViewer,
  TokenResponse,
  WorkspaceCostSummary,
} from '../generated'
import { iamSession, opsLibraryDashboard, shellViewerFixture } from './fixtures'
import { documentGridFacets, documentGridPage } from './document-grid'

type SessionResolvePayload = {
  bootstrapStatus: { setupRequired: boolean }
  locale: 'en'
  me: {
    principal: { displayLabel: string; id: string }
    user: { displayName: string; login: string }
  } | null
  message: string | null
  mode: 'authenticated' | 'guest'
  session: { expiresAt: string; id: string } | null
  shellBootstrap: {
    libraries: Array<{
      id: string
      ingestionReady: boolean
      missingBindingPurposes: string[]
      name: string
      slug: string
      workspaceId: string
    }>
    // The shell reads its coarse role from `viewer`; without it every mocked
    // session resolves to `viewer`, and the administration routes render the
    // no-access screen instead of the page under test.
    viewer: ShellViewer
    workspaces: Array<{ id: string; name: string; slug: string }>
  } | null
}

/**
 * A single revision available for download in the E2E documents-inspector
 * flow. `body` is the exact byte content served back for that revision — it
 * must differ across revisions and from the current document's `body` so a
 * test can prove a download picked the right one instead of always falling
 * back to the current source.
 */
type DocumentsMockRevision = {
  id: string
  revisionNumber: number
  body: string
  mimeType?: string
  contentSourceKind?: string
  createdAt?: string
}

export type DocumentsMockConfig = {
  libraryId?: string
  items?: ContentDocumentListItem[]
  costSummaries?: {
    library: LibraryCostSummary
    workspace: WorkspaceCostSummary
  }
  document: {
    id: string
    fileName: string | null
    fileType?: string
    mimeType?: string
    body: string
    readiness?: DocumentReadiness
    status?: DocumentStatus
    /** Fixture inventory, deliberately not a field in the document wire model. */
    vectorGap?: boolean
  }
  revisions: DocumentsMockRevision[]
  /** A revision id that 404s with a problem+json body, for the "revision no longer available" case. */
  unavailableRevisionId?: string
}

export type AiMockConfig = {
  /** Provider catalog rows; must satisfy `parseProviderCatalogResponse`. */
  providers?: ProviderCatalogEntryResponse[]
  /** Model catalog rows; must satisfy `parseModelCatalogResponse`. */
  models?: ModelCatalogEntryResponse[]
  accounts?: AiAccountResponse[]
  /** Returned per `scopeKind`: the bindings handlers filter by the query. */
  bindings?: AiBindingResponse[]
}

export type BrowserMockConfig = {
  libraries?: CatalogLibraryResponse[]
  workspaces?: NonNullable<SessionResolvePayload['shellBootstrap']>['workspaces']
  branding?: Partial<Branding>
  tokens?: TokenResponse[]
  audit?: {
    items: AuditEventResponse[]
  }
  libraryQueue?: {
    libraries: AdminLibraryPageResponse
    catalog: CatalogLibraryResponse[]
    items: IngestQueueItemResponse[]
  }
  authenticated?: boolean
  bootstrapRequired?: boolean
  dashboard?: GetLibraryDashboardResponse
  documents?: DocumentsMockConfig
  ai?: AiMockConfig
  queryConversations?: Record<string, AssistantHydratedConversation>
  querySessions?: AssistantSessionListItem[]
  session?: GetIamSessionResponse
}

declare global {
  interface Window {
    __IRONRAG_E2E_MOCKS__?: BrowserMockConfig
    __IRONRAG_MSW_READY__?: boolean
  }
}

const WORKSPACE_ID = 'workspace-alpha'
const LIBRARY_ID = 'library-demo-1'

function resolveSessionPayload(
  authenticated: boolean,
  session: GetIamSessionResponse,
  bootstrapRequired: boolean,
  libraries: BrowserMockConfig['libraries'],
  workspaces: BrowserMockConfig['workspaces'],
): SessionResolvePayload {
  if (!authenticated) {
    return {
      bootstrapStatus: { setupRequired: bootstrapRequired },
      locale: 'en',
      me: null,
      message: null,
      mode: 'guest',
      session: null,
      shellBootstrap: null,
    }
  }

  return {
    bootstrapStatus: { setupRequired: false },
    locale: 'en',
    me: {
      principal: {
        displayLabel: session.user.displayName,
        id: session.user.principalId,
      },
      user: {
        displayName: session.user.displayName,
        login: session.user.login,
      },
    },
    message: null,
    mode: 'authenticated',
    session: {
      expiresAt: session.expiresAt,
      id: session.sessionId,
    },
    shellBootstrap: {
      libraries: libraries?.map((library) => ({
        id: library.id,
        name: library.displayName,
        slug: library.slug,
        workspaceId: library.workspaceId,
        ingestionReady: library.ingestionReadiness.ready,
        missingBindingPurposes: library.ingestionReadiness.missingBindingPurposes,
      })) ?? [
        {
          id: LIBRARY_ID,
          ingestionReady: true,
          missingBindingPurposes: [],
          name: 'Default Library',
          slug: 'default-library',
          workspaceId: WORKSPACE_ID,
        },
      ],
      viewer: shellViewerFixture,
      workspaces: workspaces ?? [
        {
          id: WORKSPACE_ID,
          name: 'Default Workspace',
          slug: 'default',
        },
      ],
    },
  }
}

function buildDocumentListItem(
  documents: DocumentsMockConfig,
  libraryId: string,
): ContentDocumentListItem {
  return {
    cost: '0.00',
    costCurrencyCode: 'USD',
    documentState: 'active',
    externalKey: documents.document.id,
    fileName: documents.document.fileName,
    fileSize: documents.document.fileName === null ? null : documents.document.body.length,
    fileType: documents.document.fileName === null ? null : (documents.document.fileType ?? null),
    id: documents.document.id,
    libraryId,
    readiness: documents.document.readiness ?? 'graph_ready',
    retryable: false,
    status: documents.document.status ?? 'ready',
    uploadedAt: '2026-08-20T09:00:00.000Z',
    workspaceId: WORKSPACE_ID,
  }
}

function buildDocumentDetail(
  documents: DocumentsMockConfig,
  libraryId: string,
): ContentDocumentDetailResponse {
  return {
    document: {
      created_at: '2026-08-20T09:00:00.000Z',
      document_state: 'active',
      external_key: documents.document.id,
      id: documents.document.id,
      library_id: libraryId,
      workspace_id: WORKSPACE_ID,
    },
    fileName: documents.document.fileName,
    pipeline: {},
  }
}

function buildRevisionRows(documents: DocumentsMockConfig, libraryId: string): ContentRevision[] {
  return documents.revisions.map((revision) => ({
    byte_size: revision.body.length,
    checksum: `sha256:${revision.id}`,
    content_source_kind: revision.contentSourceKind ?? 'upload',
    created_at: revision.createdAt ?? '2026-08-20T09:00:00.000Z',
    document_id: documents.document.id,
    id: revision.id,
    library_id: libraryId,
    mime_type: revision.mimeType ?? 'text/plain',
    revision_number: revision.revisionNumber,
    workspace_id: WORKSPACE_ID,
  }))
}

function documentIdScopeError(query: URLSearchParams) {
  const ids = query.get('ids')
  if (ids === null || ids.split(',').some((id) => id.trim())) return null
  return HttpResponse.json(
    { title: 'Bad Request', status: 400, detail: 'ids filter must name at least one document' },
    { status: 400 },
  )
}

function createDocumentsHandlers(documents: DocumentsMockConfig | undefined): HttpHandler[] {
  if (!documents) return []
  const libraryId = documents.libraryId ?? LIBRARY_ID

  return [
    http.get('/v1/content/libraries/:libraryId/documents', ({ request }) => {
      const item = buildDocumentListItem(documents, libraryId)
      const query = new URL(request.url).searchParams
      const scopeError = documentIdScopeError(query)
      if (scopeError) return scopeError
      if (documents.items) return HttpResponse.json(documentGridPage(documents.items, query))
      const indexing = query.get('indexing')
      const matchesIndexing =
        !indexing ||
        (item.status === 'ready' &&
          ((indexing === 'vector_gap' && documents.document.vectorGap === true) ||
            (indexing === 'graph_gap' && item.readiness !== 'graph_ready') ||
            (indexing === 'graph_ready' && item.readiness === 'graph_ready')))
      const statuses = query.get('status')?.split(',')
      const ids = query.get('ids')?.split(',')
      const items =
        matchesIndexing &&
        (!statuses || statuses.includes(item.status)) &&
        (!ids || ids.includes(item.id))
          ? [item]
          : []
      return HttpResponse.json({
        items,
        snapshotAt: query.get('snapshotAt') ?? '2026-10-01T12:00:00.000Z',
        nextCursor: null,
        statusCounts: {
          canceled: 0,
          empty: 0,
          failed: 0,
          processing: 0,
          queued: 0,
          ready: 0,
          total: 1,
          [item.status]: 1,
        },
        totalCount: items.length,
        costRollupFreshness: 'current',
        scopePending: false,
      } satisfies DocumentListPageResponse)
    }),
    http.get('/v1/content/libraries/:libraryId/documents/facets', ({ request }) => {
      const query = new URL(request.url).searchParams
      const scopeError = documentIdScopeError(query)
      if (scopeError) return scopeError
      return HttpResponse.json(
        documentGridFacets(documents.items ?? [buildDocumentListItem(documents, libraryId)], query),
      )
    }),
    http.get('/v1/content/documents/:documentId', ({ params }) => {
      if (String(params.documentId) !== documents.document.id) {
        return HttpResponse.json({ message: 'document not found' }, { status: 404 })
      }
      return HttpResponse.json(buildDocumentDetail(documents, libraryId))
    }),
    http.get('/v1/content/documents/:documentId/revisions', ({ params }) => {
      if (String(params.documentId) !== documents.document.id) return HttpResponse.json([])
      return HttpResponse.json(buildRevisionRows(documents, libraryId))
    }),
    http.get('/v1/content/documents/:documentId/source', ({ params, request }) => {
      if (String(params.documentId) !== documents.document.id) {
        return new HttpResponse(null, { status: 404 })
      }
      const revisionId = new URL(request.url).searchParams.get('revisionId')
      if (revisionId && revisionId === documents.unavailableRevisionId) {
        // Mirrors the backend's RFC 9457 problem+json shape (see
        // `apps/api/src/interfaces/http/router_support/problem_response.rs`).
        // `detail` carries a canary string: the frontend must never surface
        // it directly, only the localized fallback toast.
        return HttpResponse.json(
          {
            type: 'about:blank',
            title: 'Not Found',
            status: 404,
            detail: 'CANARY-RAW-BACKEND-DETAIL-DO-NOT-DISPLAY',
            code: 'revision_not_found',
            request_id: 'req-e2e-canary',
          },
          { status: 404 },
        )
      }
      if (!revisionId) {
        return new HttpResponse(documents.document.body, {
          headers: { 'Content-Type': documents.document.mimeType ?? 'text/plain' },
        })
      }
      const revision = documents.revisions.find((item) => item.id === revisionId)
      if (!revision) return new HttpResponse(null, { status: 404 })
      return new HttpResponse(revision.body, {
        headers: { 'Content-Type': revision.mimeType ?? 'text/plain' },
      })
    }),
    // The documents page unconditionally derives an active-run count from
    // this endpoint's payload (`web.webRuns.filter(...)`); the generated
    // fallback handler returns `{}`, which is not an array and breaks that
    // derivation, so a documents-scoped mock must cover it too.
    http.get('/v1/content/web-runs', () =>
      HttpResponse.json([] satisfies ListContentWebIngestRunsResponse),
    ),
    // `CostCell` always renders (only CSS-hides the strip when nothing was
    // spent) and passes `costCoverage`/`currencyCode` straight into
    // `Intl.NumberFormat`; the generated `{}` fallback for these two
    // endpoints leaves `currencyCode` undefined and throws. `unknown`
    // coverage short-circuits `formatCost` before it touches `currencyCode`.
    http.get('/v1/billing/libraries/:libraryId/cost-summary', () =>
      HttpResponse.json(
        documents.costSummaries?.library ??
          ({
            costCoverage: 'unknown',
            costRollupFreshness: 'current',
            currencyCode: 'USD',
            documentCount: 1,
            providerCallCount: 0,
            totalCost: '0',
            unpricedProviderCallCount: 0,
          } satisfies LibraryCostSummary),
      ),
    ),
    http.get('/v1/billing/workspaces/:workspaceId/cost-summary', () =>
      HttpResponse.json(
        documents.costSummaries?.workspace ??
          ({
            costCoverage: 'unknown',
            costRollupFreshness: 'current',
            currencyCode: 'USD',
            documentCount: 1,
            libraryCount: 1,
            providerCallCount: 0,
            totalCost: '0',
            unpricedProviderCallCount: 0,
          } satisfies WorkspaceCostSummary),
      ),
    ),
  ]
}

/**
 * Handlers for the AI administration endpoints the bindings screen reads.
 *
 * The list endpoints share one path and differ only by the `scopeKind` query
 * parameter, so the bindings handler filters by it: returning the same rows for
 * every scope would make the library-scope screen look like it already held a
 * local binding and hide the "Override" action under test.
 */
function createAiHandlers(ai: AiMockConfig | undefined): HttpHandler[] {
  if (!ai) return []
  const providers = ai.providers ?? []
  const models = ai.models ?? []
  const accounts = ai.accounts ?? []
  const bindings = ai.bindings ?? []

  return [
    http.get('/v1/ai/providers', () => HttpResponse.json(providers)),
    http.get('/v1/ai/models', () => HttpResponse.json(models)),
    http.get('/v1/ai/accounts', () => HttpResponse.json(accounts)),
    http.get('/v1/ai/accounts/:accountId/available-models', () => HttpResponse.json([])),
    http.get('/v1/ai/bindings', ({ request }) => {
      const scopeKind = new URL(request.url).searchParams.get('scopeKind')
      return HttpResponse.json(
        scopeKind ? bindings.filter((binding) => binding.scopeKind === scopeKind) : bindings,
      )
    }),
  ]
}

function createTokenHandlers(tokens: TokenResponse[] | undefined): HttpHandler[] {
  if (!tokens) return []
  return [
    http.get('/v1/iam/tokens', ({ request }) => {
      const workspaceId = new URL(request.url).searchParams.get('workspaceId')
      return HttpResponse.json(
        tokens.filter((token) => !workspaceId || token.scope.workspace?.id === workspaceId),
      )
    }),
    http.get('/v1/iam/tokens/:tokenId', ({ params }) => {
      const token = tokens.find((item) => item.principalId === String(params.tokenId))
      return token
        ? HttpResponse.json(token)
        : HttpResponse.json({ title: 'Token not found', status: 404 }, { status: 404 })
    }),
  ]
}

/** Facet values a library column would compute, derived from the fixture rows. */
function libraryFacetValue(
  item: AdminLibraryPageResponse['items'][number],
  column: string,
): Omit<AdminLibraryFacetValue, 'count'> {
  if (column === 'cost') {
    const cost = item.costValue
    return cost
      ? {
          decimal: cost.amount,
          currencyCode: cost.currencyCode,
          coverage: cost.coverage,
          unpricedCallCount: cost.unpricedCallCount,
        }
      : {}
  }
  if (column === 'readiness') return { flag: item.ingestionReadiness.ready }
  if (column === 'library') return { text: item.displayName }
  if (column === 'workspace') return { text: item.workspace.displayName }
  if (column === 'lifecycle') return { text: item.lifecycleState }
  let number: number | null | undefined
  switch (column) {
    case 'queue':
      number = item.queuedJobCount
      break
    case 'documents':
      number = item.costSummary?.documentCount
      break
    case 'calls':
      number = item.costSummary?.providerCallCount
      break
    default:
      return {}
  }
  return number == null ? {} : { number }
}

function libraryFacetValues(items: AdminLibraryPageResponse['items'], column: string) {
  const buckets = new Map<string, AdminLibraryFacetValue>()
  for (const item of items) {
    const value = libraryFacetValue(item, column)
    const key = JSON.stringify(value)
    const bucket = buckets.get(key)
    if (bucket) bucket.count += 1
    else buckets.set(key, { ...value, count: 1 })
  }
  return [...buckets.values()]
}

function libraryFacetMatches(value: AdminLibraryFacetValue, query: URLSearchParams): boolean {
  const raw = query.get('facetSearch')?.toLowerCase() ?? ''
  const hints = JSON.parse(query.get('facetSearchHints') ?? '{}') as AdminLibrarySearchHints
  if (!raw && Object.keys(hints).length === 0) return true
  const slots = [
    value.text,
    value.number,
    value.decimal,
    value.flag,
    value.currencyCode,
    value.coverage,
    value.unpricedCallCount,
  ]
  if (raw && slots.some((slot) => slot != null && String(slot).toLowerCase().includes(raw)))
    return true
  if (
    hints.unavailable &&
    value.text == null &&
    value.number == null &&
    value.decimal == null &&
    value.flag == null
  )
    return true
  if (value.number != null && hints.numbers?.includes(value.number)) return true
  if (value.flag != null && hints.readiness?.includes(value.flag)) return true
  if (value.text != null && hints.lifecycle?.includes(value.text)) return true
  return (
    value.decimal != null &&
    Boolean(
      hints.costs?.some(
        (term) =>
          (term.amount == null || term.amount === value.decimal) &&
          (term.currencyCode == null || term.currencyCode === value.currencyCode) &&
          (term.coverage == null || term.coverage === value.coverage) &&
          (term.unpricedCallCount == null || term.unpricedCallCount === value.unpricedCallCount),
      ),
    )
  )
}

function createCatalogHandlers(libraries: CatalogLibraryResponse[] | undefined): HttpHandler[] {
  if (!libraries) return []
  return [
    http.get('/v1/catalog/workspaces/:workspaceId/libraries', ({ params, request }) => {
      const query = new URL(request.url).searchParams
      const needle = (query.get('search') ?? '').toLowerCase()
      const items = libraries.filter(
        (library) =>
          library.workspaceId === params.workspaceId &&
          library.displayName.toLowerCase().includes(needle),
      )
      return HttpResponse.json({
        items: items.slice(0, Number(query.get('limit') ?? 20)),
        totalCount: items.length,
      })
    }),
    http.get('/v1/catalog/libraries/:libraryId', ({ params }) => {
      const library = libraries.find((item) => item.id === String(params.libraryId))
      return library
        ? HttpResponse.json(library)
        : HttpResponse.json({ message: 'Library not found' }, { status: 404 })
    }),
  ]
}

function createLibraryQueueHandlers(fixture: BrowserMockConfig['libraryQueue']): HttpHandler[] {
  if (!fixture) return []
  return [
    http.get('/v1/admin/libraries', () => HttpResponse.json(fixture.libraries)),
    http.get('/v1/admin/libraries/facets', ({ request }) => {
      const query = new URL(request.url).searchParams
      const column = query.get('facetColumn') ?? 'library'
      return HttpResponse.json({
        column,
        values: libraryFacetValues(fixture.libraries.items, column).filter((value) =>
          libraryFacetMatches(value, query),
        ),
        nextCursor: null,
      })
    }),
    http.get('/v1/ingest/queue', ({ request }) => {
      const query = new URL(request.url).searchParams
      const items = fixture.items.filter(
        (item) =>
          (!query.get('libraryId') || item.libraryId === query.get('libraryId')) &&
          (!query.get('workspaceId') || item.workspaceId === query.get('workspaceId')),
      )
      return HttpResponse.json({
        items,
        summary: {
          queued: items.filter((item) => item.queueState === 'queued').length,
          paused: items.filter((item) => item.queueState === 'paused').length,
          running: items.filter(
            (item) => item.queueState === 'leased' || item.queueState === 'running',
          ).length,
          total: items.length,
        },
      })
    }),
  ]
}

function auditActorLabel(item: AuditEventResponse): string {
  const actor = item.actorPrincipal
  if (!actor) return item.actorPrincipalId ?? ''
  return actor.displayLabel.trim() || actor.displayName?.trim() || actor.login?.trim() || actor.id
}

function auditFacetValue(
  item: AuditEventResponse,
  column: string,
): { value: string | null; label: string } {
  switch (column) {
    case 'actor':
      return { value: item.actorPrincipalId ?? null, label: auditActorLabel(item) || 'system' }
    case 'action':
      return { value: item.actionKind, label: item.actionKind }
    case 'surface':
      return { value: item.surfaceKind, label: item.surfaceKind }
    case 'result':
      return { value: item.resultKind, label: item.resultKind }
    case 'created_at':
      return { value: item.createdAt, label: item.createdAt }
    default:
      return { value: null, label: '' }
  }
}

function auditHintMatches(item: AuditEventResponse, raw: string | null, column?: string): boolean {
  if (!raw) return false
  const hints = JSON.parse(raw) as AuditSearchHintsInput
  return (
    ((!column || column === 'action') && !!hints.actionKinds?.includes(item.actionKind)) ||
    ((!column || column === 'surface') && !!hints.surfaceKinds?.includes(item.surfaceKind)) ||
    ((!column || column === 'result') && !!hints.resultKinds?.includes(item.resultKind)) ||
    ((!column || column === 'actor') && !!hints.actorNull && item.actorPrincipalId === null) ||
    ((!column || column === 'created_at') &&
      !!hints.createdAtRange &&
      Date.parse(item.createdAt) >= Date.parse(hints.createdAtRange.min) &&
      Date.parse(item.createdAt) < Date.parse(hints.createdAtRange.max))
  )
}

/** Text search and hint match over one audit event. */
function auditSearchMatches(item: AuditEventResponse, query: URLSearchParams): boolean {
  const search = (query.get('search') ?? '').toLowerCase()
  if (!search && !query.has('searchHints')) return true
  const haystack = [
    item.actionKind,
    auditActorLabel(item),
    item.surfaceKind,
    item.resultKind,
    item.redactedMessage ?? '',
    item.createdAt,
  ]
    .join(' ')
    .toLowerCase()
  if (search && haystack.includes(search)) return true
  return auditHintMatches(item, query.get('searchHints'))
}

/** The actor filter; `excludeColumn` drops it for the actor facet itself. */
function auditActorMatches(
  item: AuditEventResponse,
  query: URLSearchParams,
  excludeColumn?: string,
): boolean {
  if (excludeColumn === 'actor') return true
  const raw = query.get('actor')
  const includeNull = query.get('actorNull') === 'true'
  if (!raw && !includeNull) return true
  const ids = (raw ?? '')
    .split(',')
    .map((value) => value.trim())
    .filter(Boolean)
  return item.actorPrincipalId ? ids.includes(item.actorPrincipalId) : includeNull
}

/** The action/surface/result/created_at filters. */
function auditColumnMatches(
  item: AuditEventResponse,
  query: URLSearchParams,
  excludeColumn?: string,
): boolean {
  const columns = [
    ['actionKind', 'action', item.actionKind],
    ['surfaceKind', 'surface', item.surfaceKind],
    ['resultKind', 'result', item.resultKind],
    ['createdAt', 'created_at', item.createdAt],
  ] as const
  for (const [param, column, value] of columns) {
    if (excludeColumn === column) continue
    const raw = query.get(param)
    if (!raw) continue
    const values = raw
      .split(',')
      .map((entry) => entry.trim())
      .filter(Boolean)
    if (!values.includes(value)) return false
  }
  return true
}

/** Whole-dataset filter; `excludeColumn` drops the facet column's own selection. */
function auditMatches(
  item: AuditEventResponse,
  query: URLSearchParams,
  excludeColumn?: string,
): boolean {
  return (
    auditSearchMatches(item, query) &&
    auditActorMatches(item, query, excludeColumn) &&
    auditColumnMatches(item, query, excludeColumn)
  )
}

function auditSortValue(item: AuditEventResponse, column: string): string | null {
  switch (column) {
    case 'actor':
      return item.actorPrincipalId ? auditActorLabel(item) : null
    case 'action':
      return item.actionKind
    case 'surface':
      return item.surfaceKind
    case 'result':
      return item.resultKind
    default:
      return item.createdAt
  }
}

/** NULL-last, stable id tie-break in both directions. */
function sortAuditItems(
  items: AuditEventResponse[],
  sortBy: string | null,
  sortOrder: string | null,
): AuditEventResponse[] {
  const column = sortBy ?? 'created_at'
  const direction = sortOrder === 'asc' ? 1 : -1
  return [...items].sort((left, right) => {
    const a = auditSortValue(left, column)
    const b = auditSortValue(right, column)
    if (a === null && b !== null) return 1
    if (a !== null && b === null) return -1
    if (a !== null && b !== null && a !== b) return a < b ? -direction : direction
    if (left.id === right.id) return 0
    return left.id < right.id ? -direction : direction
  })
}

function createAuditHandlers(fixture: BrowserMockConfig['audit']): HttpHandler[] {
  if (!fixture) return []
  return [
    http.get('/v1/audit/events/facets', ({ request }) => {
      const query = new URL(request.url).searchParams
      const column = query.get('facetColumn') ?? 'actor'
      const search = (query.get('facetSearch') ?? '').toLowerCase()
      const counts = new Map<string, { value: string | null; label: string; count: number }>()
      for (const item of fixture.items) {
        if (!auditMatches(item, query, column)) continue
        const { value, label } = auditFacetValue(item, column)
        const haystack = `${label} ${value ?? ''}`.toLowerCase()
        if (
          (search || query.has('facetSearchHints')) &&
          !(search && haystack.includes(search)) &&
          !auditHintMatches(item, query.get('facetSearchHints'), column)
        )
          continue
        const selected = query.get('selectedValue')?.split(',')
        if (selected?.length && !selected.includes(value ?? '')) continue
        const existing = counts.get(value ?? '\u0000null')
        if (existing) existing.count += 1
        else counts.set(value ?? '\u0000null', { value, label, count: 1 })
      }
      const all = [...counts.values()].sort(
        (left, right) =>
          right.count - left.count || (left.value ?? '').localeCompare(right.value ?? ''),
      )
      const limit = Number(query.get('facetLimit') ?? '50')
      const offset = Number(query.get('facetOffset') ?? '0')
      return HttpResponse.json({
        values: all.slice(offset, offset + limit),
        hasMore: offset + limit < all.length,
      })
    }),
    http.get('/v1/audit/events', ({ request }) => {
      const query = new URL(request.url).searchParams
      const filtered = fixture.items.filter((item) => auditMatches(item, query))
      const sorted = sortAuditItems(filtered, query.get('sortBy'), query.get('sortOrder'))
      const limit = Number(query.get('limit') ?? '50')
      const start = Number(query.get('cursor') ?? '0')
      const items = sorted.slice(start, start + limit)
      const next = start + limit < sorted.length ? String(start + limit) : null
      return HttpResponse.json({ items, total: sorted.length, nextCursor: next })
    }),
  ]
}

export function createBrowserMockHandlers(config: BrowserMockConfig = {}): HttpHandler[] {
  const session = config.session ?? iamSession()
  const dashboard = config.dashboard ?? opsLibraryDashboard()
  const queryConversations = { ...config.queryConversations }
  let querySessions = [...(config.querySessions ?? [])]
  let bootstrapRequired = config.bootstrapRequired ?? false
  let authenticated = config.authenticated ?? false
  const bootstrapStatus = () => ({
    setupRequired: bootstrapRequired,
    aiSetup: bootstrapRequired
      ? {
          bindingBundles: [
            {
              providerCatalogId: 'provider-hosted-router',
              providerKind: 'hosted-router',
              displayName: 'Hosted Router',
              credentialSource: 'missing',
              defaultBaseUrl: 'https://router.example/api/v1',
              credentialPolicy: {
                apiKeyRequired: true,
                baseUrlRequired: false,
                baseUrlMode: 'fixed',
                validationMode: 'model_list',
              },
              baseUrlPolicy: {
                allowOverride: false,
                requireHttps: true,
                allowPrivateNetwork: false,
                trimSuffixes: [],
              },
              modelDiscovery: {
                mode: 'credential',
                paths: [
                  { capabilityKind: 'chat', path: '/models' },
                  { capabilityKind: 'embedding', path: '/embeddings/models' },
                ],
              },
              capabilities: {},
              runtime: {
                kind: 'openai_compatible',
                authScheme: 'bearer',
                chatPath: '/chat/completions',
                embeddingsPath: '/embeddings',
                modelsPath: '/models',
                structuredOutput: 'json_object',
                tokenLimitParameter: 'max_tokens',
              },
              uiHints: {},
              bindings: [
                {
                  bindingPurpose: 'extract_graph',
                  modelCatalogId: 'model-hosted-router-chat',
                  modelName: 'hosted/chat-small',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'embed_chunk',
                  modelCatalogId: 'model-hosted-router-embedding',
                  modelName: 'hosted/text-embedding-small',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'query_compile',
                  modelCatalogId: 'model-hosted-router-chat',
                  modelName: 'hosted/chat-small',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'query_answer',
                  modelCatalogId: 'model-hosted-router-answer',
                  modelName: 'hosted/chat-answer',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'extract_text',
                  modelCatalogId: 'model-hosted-router-answer',
                  modelName: 'hosted/chat-answer',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'agent',
                  modelCatalogId: 'model-hosted-router-answer',
                  modelName: 'hosted/chat-answer',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
              ],
            },
            {
              providerCatalogId: 'provider-local-runtime',
              providerKind: 'local-runtime',
              displayName: 'Local Runtime',
              credentialSource: 'missing',
              defaultBaseUrl: 'http://127.0.0.1:18080/v1',
              credentialPolicy: {
                apiKeyRequired: false,
                baseUrlRequired: true,
                baseUrlMode: 'required',
                validationMode: 'model_list',
              },
              baseUrlPolicy: {
                allowOverride: true,
                requireHttps: false,
                allowPrivateNetwork: true,
                trimSuffixes: ['/v1'],
              },
              modelDiscovery: {
                mode: 'credential',
                paths: [
                  { capabilityKind: 'chat', path: '/models' },
                  { capabilityKind: 'embedding', path: '/models' },
                ],
              },
              capabilities: {},
              runtime: {
                kind: 'openai_compatible',
                authScheme: 'none',
                chatPath: '/chat/completions',
                embeddingsPath: '/embeddings',
                modelsPath: '/models',
                structuredOutput: 'json_object',
                tokenLimitParameter: 'max_tokens',
              },
              uiHints: {},
              bindings: [
                {
                  bindingPurpose: 'extract_graph',
                  modelCatalogId: 'model-local-runtime-chat',
                  modelName: 'local-chat-small',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'embed_chunk',
                  modelCatalogId: 'model-local-runtime-embedding',
                  modelName: 'local-embedding-small',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'query_compile',
                  modelCatalogId: 'model-local-runtime-chat',
                  modelName: 'local-chat-small',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'query_answer',
                  modelCatalogId: 'model-local-runtime-chat',
                  modelName: 'local-chat-small',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'extract_text',
                  modelCatalogId: 'model-local-runtime-vision',
                  modelName: 'local-vision-small',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'agent',
                  modelCatalogId: 'model-local-runtime-chat',
                  modelName: 'local-chat-small',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
              ],
            },
          ],
        }
      : null,
  })

  return [
    http.get('/v1/iam/context', () =>
      HttpResponse.json(
        resolveSessionPayload(
          authenticated,
          session,
          bootstrapRequired,
          config.libraries,
          config.workspaces,
        ),
      ),
    ),
    http.get('/v1/iam/bootstrap', () => HttpResponse.json(bootstrapStatus())),
    http.get('/v1/ready', () =>
      HttpResponse.json({
        checkedAt: '2026-05-05T15:30:00.000Z',
        status: 'ready',
        role: 'api',
        dependencies: {
          postgres: { mode: 'postgres', status: 'ok' },
          redis: { mode: 'redis', status: 'ok' },
          knowledgePlane: { mode: 'postgres', status: 'ok' },
        },
        startupAuthority: { mode: 'standalone', state: 'succeeded' },
        storage: { provider: 'local', topology: 'single_node', status: 'ok' },
        topology: { status: 'supported' },
      } satisfies DeploymentReadinessSnapshot),
    ),
    http.get('/v1/mcp/capabilities', () =>
      HttpResponse.json({
        tokenKind: 'system',
        toolContractHash: 'fixture-contract',
        toolContractVersion: 1,
        tools: [],
        visibleLibraryCount: 1,
        visibleWorkspaceCount: 1,
      } satisfies McpCapabilitySnapshot),
    ),
    http.get('/v1/query/system-prompt', () =>
      HttpResponse.json({
        template: 'Use the configured library to ground answers.',
      } satisfies AssistantSystemPromptResponse),
    ),
    http.post('/v1/iam/bootstrap', async () => {
      bootstrapRequired = false
      authenticated = true
      return HttpResponse.json(session)
    }),
    http.post('/v1/iam/sessions', () => {
      authenticated = true
      return HttpResponse.json(session)
    }),
    http.post('/v1/iam/session', () => {
      authenticated = false
      return HttpResponse.json({})
    }),
    http.get('/v1/ops/libraries/:libraryId/dashboard', () => HttpResponse.json(dashboard)),
    http.get('/v1/query/libraries/:libraryId/sessions', () =>
      HttpResponse.json({
        items: querySessions,
        nextCursor: null,
        total: querySessions.length,
      } satisfies ListQuerySessionsResponse),
    ),
    http.patch('/v1/query/sessions/:sessionId', async ({ params, request }) => {
      const sessionId = String(params.sessionId)
      const current = querySessions.find((session) => session.id === sessionId)
      if (!current) {
        return HttpResponse.json({ message: 'query session not found' }, { status: 404 })
      }

      const payload: unknown = await request.json()
      const rawTitle =
        typeof payload === 'object' && payload !== null && 'title' in payload
          ? (payload as { title?: unknown }).title
          : undefined
      const title = typeof rawTitle === 'string' ? rawTitle.trim().replaceAll(/\s+/gu, ' ') : ''
      if (!title || Array.from(title).length > 72) {
        return HttpResponse.json({ message: 'invalid query session title' }, { status: 400 })
      }

      const updatedAt = new Date().toISOString()
      querySessions = querySessions.map((session) =>
        session.id === sessionId ? { ...session, title, updatedAt } : session,
      )
      const hydrated = queryConversations[sessionId]
      if (hydrated) {
        queryConversations[sessionId] = {
          ...hydrated,
          session: { ...hydrated.session, title, updatedAt },
        }
      }

      return HttpResponse.json({ ...current, title, updatedAt })
    }),
    http.delete('/v1/query/sessions/:sessionId', ({ params }) => {
      const sessionId = String(params.sessionId)
      const exists = querySessions.some((session) => session.id === sessionId)
      if (!exists) {
        return HttpResponse.json({ message: 'query session not found' }, { status: 404 })
      }
      querySessions = querySessions.filter((session) => session.id !== sessionId)
      delete queryConversations[sessionId]
      return new HttpResponse(null, { status: 204 })
    }),
    http.get('/v1/query/sessions/:sessionId', ({ params }) => {
      const conversation = queryConversations[String(params.sessionId)]
      return conversation
        ? HttpResponse.json(conversation)
        : HttpResponse.json({ message: 'query session not found' }, { status: 404 })
    }),
    http.get('/v1/version/latest', () =>
      HttpResponse.json({
        checkedAt: '2026-05-05T15:30:00.000Z',
        currentVersion: '0.4.1',
        latestVersion: null,
        releaseUrl: null,
        repositoryUrl: 'https://github.com/mlimarenko/IronRAG',
        status: 'up_to_date',
      }),
    ),
    ...(config.branding !== undefined
      ? [http.get('/branding.json', () => HttpResponse.json(config.branding))]
      : []),
    ...createDocumentsHandlers(config.documents),
    ...createAuditHandlers(config.audit),
    ...createCatalogHandlers(config.libraries ?? config.libraryQueue?.catalog),
    ...createLibraryQueueHandlers(config.libraryQueue),
    ...createTokenHandlers(config.tokens),
    ...createAiHandlers(config.ai),
  ]
}
