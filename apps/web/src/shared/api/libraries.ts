import { infiniteQueryOptions } from '@tanstack/react-query'

import { Admin } from './generated'
import { unwrap } from './runtime'
import type {
  AdminLibraryColumnFilters,
  AdminLibraryFacetColumn,
  AdminLibraryFacetsResponse,
  AdminLibrarySearchHints,
  AdminLibrarySortKey,
  AdminLibrarySortOrder,
  ListAdminLibraryFacetsData,
  ListAdminLibrariesData,
} from './generated'

export type { AdminLibraryColumnFilters, AdminLibraryFacetColumn, AdminLibraryFacetsResponse }

export type { AdminLibrarySearchHints }

/** All facet scopes share this prefix so mutations invalidate active and inactive pages. */
export const ADMIN_LIBRARY_FACETS_QUERY_KEY = ['adminLibraryFacets'] as const

/** The params of one administration library page. */
export interface AdminLibraryListParams {
  search?: string
  /** An Intl-derived canonical spelling of `search`, OR'd over the same text columns. */
  normalizedSearch?: string | null
  /** Typed generic hints over the non-text columns, OR'd with the text search. */
  searchHints?: AdminLibrarySearchHints | null
  /**
   * The grid's per-column filters. Serialized to JSON here, at the one owning
   * boundary, so no caller hand-writes the wire string. NULL elements are real
   * (the column's own NULL bucket) and a decimal cost travels as its exact
   * source text.
   */
  filters?: AdminLibraryColumnFilters
  sortBy?: AdminLibrarySortKey
  sortOrder?: AdminLibrarySortOrder
  offset?: number
  limit?: number
}

/** One serializer owns the search, filters and sort of every library reader. */
export function adminLibraryListQuery(
  params: AdminLibraryListParams,
): NonNullable<ListAdminLibrariesData['query']> {
  const query: NonNullable<ListAdminLibrariesData['query']> = {}
  if (params.search !== undefined) query.search = params.search
  if (params.normalizedSearch) query.normalizedSearch = params.normalizedSearch
  if (params.searchHints) query.searchHints = JSON.stringify(params.searchHints)
  if (params.filters) query.filters = JSON.stringify(params.filters)
  if (params.sortBy !== undefined) query.sortBy = params.sortBy
  if (params.sortOrder !== undefined) query.sortOrder = params.sortOrder
  if (params.offset !== undefined) query.offset = params.offset
  if (params.limit !== undefined) query.limit = params.limit
  return query
}

/** The params of one administration library facet page. */
export interface AdminLibraryFacetParams {
  column: AdminLibraryFacetColumn
  /** The same whole-table search the list carries. */
  search?: string
  normalizedSearch?: string | null
  searchHints?: AdminLibrarySearchHints | null
  /** The same per-column filters the list carries; the selected column self-excludes. */
  filters?: AdminLibraryColumnFilters
  /** Server-side search across this column's own displayed values. */
  facetSearch?: string
  /**
   * The typed hints the column's own `facetSearch` derives, scoped to that
   * column and derived independently of the whole-table `searchHints`.
   */
  facetSearchHints?: AdminLibrarySearchHints | null
  facetCursor?: string
  facetLimit?: number
}

function adminLibraryFacetRequest(
  params: AdminLibraryFacetParams,
): NonNullable<ListAdminLibraryFacetsData['query']> {
  const query: NonNullable<ListAdminLibraryFacetsData['query']> = {
    facetColumn: params.column,
  }
  if (params.search !== undefined) query.search = params.search
  if (params.normalizedSearch) query.normalizedSearch = params.normalizedSearch
  if (params.searchHints) query.searchHints = JSON.stringify(params.searchHints)
  if (params.filters) query.filters = JSON.stringify(params.filters)
  if (params.facetSearch !== undefined) query.facetSearch = params.facetSearch
  if (params.facetSearchHints) query.facetSearchHints = JSON.stringify(params.facetSearchHints)
  if (params.facetCursor !== undefined) query.facetCursor = params.facetCursor
  if (params.facetLimit !== undefined) query.facetLimit = params.facetLimit
  return query
}

/**
 * Distinct values of one library grid column, keyset-paginated on the canonical
 * value. The server self-excludes the column's own filter and applies every
 * other filter/search predicate, so a count and the page it opens cannot
 * disagree; the client never computes this column from one page.
 */
export function adminLibraryFacetsInfiniteOptions(params: AdminLibraryFacetParams) {
  const initialPageParam: { cursor: string | null } = { cursor: null }
  return infiniteQueryOptions({
    queryKey: [...ADMIN_LIBRARY_FACETS_QUERY_KEY, params],
    initialPageParam,
    queryFn: ({ pageParam, signal }) =>
      Admin.listAdminLibraryFacets({
        query: adminLibraryFacetRequest({
          ...params,
          ...(pageParam.cursor ? { facetCursor: pageParam.cursor } : {}),
        }),
        signal,
      }).then((result): AdminLibraryFacetsResponse => unwrap(result)),
    getNextPageParam: (last: AdminLibraryFacetsResponse) =>
      last.nextCursor ? { cursor: last.nextCursor } : undefined,
  })
}
