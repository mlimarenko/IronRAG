import { infiniteQueryOptions } from '@tanstack/react-query'

import { Audit } from './generated'
import type {
  AuditEventFacetsResponse,
  AuditEventPageResponse,
  AuditSearchHintsInput,
  ListAuditEventFacetsData,
  ListAuditEventsData,
} from './generated'
import { unwrap } from './runtime'

export type AuditEventPage = AuditEventPageResponse
export type AuditFacetsResponse = AuditEventFacetsResponse
export type AuditFacetValue = AuditEventFacetsResponse['values'][number]

/**
 * The audit grid columns. Ids are the server's own vocabulary: the `sortBy`
 * values, the `facetColumn` values and the query-parameter names all agree on
 * `actor`, `action`, `surface`, `result` and `created_at`, so a column id maps
 * to a facet request and a server sort without a lookup table that could drift.
 */
export type AuditGridColumnId = 'actor' | 'action' | 'surface' | 'result' | 'created_at'
export type AuditSortOrder = 'asc' | 'desc'

/**
 * The typed per-column filters the list and facet endpoints take. Every value
 * is decoded from its typed grid facet key, so a null actor bucket stays a real
 * flag and a timestamp keeps the exact text it arrived with.
 */
export interface AuditColumnFilters {
  actor?: string[]
  actorNull?: boolean
  actionKind?: string[]
  surfaceKind?: string[]
  resultKind?: string[]
  createdAt?: string[]
}

export interface AuditListParams {
  workspaceId?: string
  libraryId?: string
  internal?: boolean
  includeAssistant?: boolean
  search?: string
  sortBy?: AuditGridColumnId
  sortOrder?: AuditSortOrder
  cursor?: string
  limit?: number
  filters?: AuditColumnFilters
  /**
   * Derived search hints: closed-enum values and a UTC date interval whose
   * *displayed* spelling matches the typed term. The backend ORs them over the
   * same vocabulary the grid renders, so a reader searches what they see.
   */
  searchHints?: AuditSearchHints
}

/**
 * Derived, bounded search hints. The canonical wire shape is owned by the
 * generated contract (`AuditSearchHintsInput`); this domain name aliases it so
 * existing consumers keep a stable import. Each list is OR'd over the visible
 * column, `actorNull` includes the no-actor rows, and `createdAtRange` is the
 * UTC interval a localized date/time term resolved to (`null` when unset).
 */
export type AuditSearchHints = AuditSearchHintsInput

/** One selected value's canonical key and server label, resolved by key. */
export interface AuditSelectedValue {
  column: AuditGridColumnId
  value: string
  label: string
}

export interface AuditFacetParams extends Omit<
  AuditListParams,
  'sortBy' | 'sortOrder' | 'cursor' | 'limit'
> {
  facetColumn: AuditGridColumnId
  /** Selected values whose labels this column's scoped facet lookup resolves. */
  selectedValues?: AuditSelectedValue[]
  facetSearch?: string
  /**
   * The hints of the facet's *own* value search, resolved against the opened
   * column's formatter only. They travel separately from `searchHints`
   * (the global term's hints) so a local menu search can never overwrite the
   * whole-table hints, and the facet scope is never widened by another
   * column's vocabulary.
   */
  facetSearchHints?: AuditSearchHints
  facetOffset?: number
  facetLimit?: number
}

/**
 * One serializer owns the authorized scope, the whole-table search and every
 * column filter, so the list and its facets cannot drift. The derived search
 * hints travel as one JSON `searchHints` parameter, so the server ORs them over
 * the visible columns instead of matching them as literal text. There is no
 * scalar hint parameter: the JSON shape is the canonical contract.
 */
function auditScopeQuery(
  params: AuditListParams | AuditFacetParams,
): NonNullable<ListAuditEventsData['query']> {
  const query: NonNullable<ListAuditEventsData['query']> = {}
  if (params.workspaceId) query.workspaceId = params.workspaceId
  if (params.libraryId) query.libraryId = params.libraryId
  if (params.internal) query.internal = true
  if (params.search) query.search = params.search
  const filters = params.filters
  if (filters?.actor?.length) query.actor = filters.actor.join(',')
  if (filters?.actorNull) query.actorNull = true
  if (filters?.actionKind?.length) query.actionKind = filters.actionKind.join(',')
  if (filters?.surfaceKind?.length) query.surfaceKind = filters.surfaceKind.join(',')
  if (filters?.resultKind?.length) query.resultKind = filters.resultKind.join(',')
  if (filters?.createdAt?.length) query.createdAt = filters.createdAt.join(',')
  const hints = serializeSearchHints(params.searchHints)
  if (hints) query.searchHints = hints
  return query
}

/**
 * The canonical JSON encoding of one bounded hint set. An empty hint set has no
 * wire representation, so a request either carries a real JSON object or omits
 * the parameter entirely.
 */
export function serializeSearchHints(hints: AuditSearchHints | undefined): string | undefined {
  if (!hints) return undefined
  const hasAny =
    Boolean(hints.actionKinds?.length) ||
    Boolean(hints.surfaceKinds?.length) ||
    Boolean(hints.resultKinds?.length) ||
    hints.actorNull === true ||
    Boolean(hints.createdAtRange)
  if (!hasAny) return undefined
  const canonical: AuditSearchHintsInput = {}
  if (hints.actionKinds?.length) canonical.actionKinds = hints.actionKinds
  if (hints.surfaceKinds?.length) canonical.surfaceKinds = hints.surfaceKinds
  if (hints.resultKinds?.length) canonical.resultKinds = hints.resultKinds
  if (hints.actorNull === true) canonical.actorNull = true
  if (hints.createdAtRange) canonical.createdAtRange = hints.createdAtRange
  return JSON.stringify(canonical)
}

export function auditListQuery(params: AuditListParams): NonNullable<ListAuditEventsData['query']> {
  const query = auditScopeQuery(params)
  if (params.includeAssistant) query.includeAssistant = true
  if (params.sortBy) query.sortBy = params.sortBy
  if (params.sortOrder) query.sortOrder = params.sortOrder
  if (params.cursor) query.cursor = params.cursor
  if (params.limit !== undefined) query.limit = params.limit
  return query
}

export function auditFacetQuery(
  params: AuditFacetParams,
): NonNullable<ListAuditEventFacetsData['query']> {
  const query: NonNullable<ListAuditEventFacetsData['query']> = {
    ...auditScopeQuery(params),
    facetColumn: params.facetColumn,
  }
  if (params.selectedValues?.length) {
    query.selectedValueColumn = params.selectedValues.map((entry) => entry.column).join(',')
    query.selectedValue = params.selectedValues.map((entry) => entry.value).join(',')
  }
  if (params.facetSearch) query.facetSearch = params.facetSearch
  // The facet's own value search is a separate hint set: it narrows only the
  // opened column and never replaces the global `searchHints` above.
  const facetHints = serializeSearchHints(params.facetSearchHints)
  if (facetHints) query.facetSearchHints = facetHints
  if (params.facetLimit !== undefined) query.facetLimit = params.facetLimit
  if (params.facetOffset !== undefined) query.facetOffset = params.facetOffset
  return query
}

export const auditApi = {
  async list(params: AuditListParams, signal?: AbortSignal): Promise<AuditEventPageResponse> {
    const result = await Audit.listAuditEvents({
      query: auditListQuery(params),
      ...(signal ? { signal } : {}),
    })
    return unwrap<AuditEventPageResponse>(result)
  },
  async facets(params: AuditFacetParams, signal?: AbortSignal): Promise<AuditEventFacetsResponse> {
    const result = await Audit.listAuditEventFacets({
      query: auditFacetQuery(params),
      ...(signal ? { signal } : {}),
    })
    return unwrap<AuditEventFacetsResponse>(result)
  },
}

/**
 * A lazily-fetched, self-excluding facet source for the shared grid. The
 * complete authorized scope is the query cache key, and TanStack Query owns the
 * cancellation: a menu closed mid-request aborts its own call.
 */
export function auditFacetsInfiniteOptions(params: AuditFacetParams) {
  const pageSize = params.facetLimit ?? 50
  return infiniteQueryOptions({
    queryKey: ['auditFacets', params],
    initialPageParam: 0,
    queryFn: ({ pageParam, signal }) =>
      auditApi.facets({ ...params, facetOffset: pageParam }, signal),
    getNextPageParam: (last: AuditEventFacetsResponse, _pages, lastParam: number) =>
      last.hasMore ? lastParam + pageSize : undefined,
  })
}
