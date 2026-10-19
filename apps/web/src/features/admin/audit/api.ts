import { keepPreviousData, useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { useCallback, useMemo, useState } from 'react'
import type { TFunction } from 'i18next'

import {
  auditApi,
  auditFacetsInfiniteOptions,
  type AuditColumnFilters,
  type AuditFacetParams,
  type AuditGridColumnId,
  type AuditSelectedValue,
} from '@/shared/api'
import {
  buildGridServerFacets,
  gridFacetKey,
  gridFacetValue,
  type GridFacetPage,
  type GridServerFacets,
  type GridState,
} from '@/shared/data-grid'
import type { Locale } from '@/shared/types'

import {
  auditFacetOptions,
  deriveAuditColumnSearch,
  deriveAuditSearch,
  mapAuditPage,
} from './model'

/** The whole authorized scope of one audit chain; it is the query cache key. */
export interface AuditScope {
  workspaceId: string
  libraryId: string
  internal: boolean
  includeAssistant: boolean
  filters: AuditColumnFilters | undefined
  search: string
  sort: GridState['sort']
  limit: number
}

/** The global scope every reader sends: identity, the whole-table search (raw
 * term plus its derived hints) and column filters. The hint derivation lives
 * here, in the one owner of the scope, so the list, the facets and the
 * selected-value labels all carry the *same* global hints and none of them can
 * silently drop or overwrite them.
 */
function scopeParams(scope: AuditScope, locale: Locale, t: TFunction) {
  const { sort } = scope
  const searchHints = deriveAuditSearch(scope.search, locale, t)
  const hasSearch = scope.search !== ''
  return {
    ...(scope.workspaceId ? { workspaceId: scope.workspaceId } : {}),
    ...(scope.libraryId ? { libraryId: scope.libraryId } : {}),
    internal: scope.internal,
    includeAssistant: scope.includeAssistant,
    ...(hasSearch ? { search: scope.search } : {}),
    ...(hasSearch && Object.keys(searchHints).length > 0 ? { searchHints } : {}),
    ...(scope.filters ? { filters: scope.filters } : {}),
    ...(sort ? { sortBy: sort.columnId as AuditGridColumnId, sortOrder: sort.direction } : {}),
  }
}

export function useAuditEvents(
  scope: AuditScope,
  cursor: string | null,
  t: TFunction,
  locale: Locale,
) {
  return useQuery({
    // The complete authorized scope is the cache key, so a widened scope can
    // never be answered from a narrower cache entry. The derivation inputs are
    // part of the key too, so a language switch refetches rather than reusing
    // labels from the previous locale.
    queryKey: ['auditEvents', scope, locale, cursor, t],
    queryFn: ({ signal }) =>
      auditApi.list(
        { ...scopeParams(scope, locale, t), limit: scope.limit, ...(cursor ? { cursor } : {}) },
        signal,
      ),
    placeholderData: keepPreviousData,
    select: mapAuditPage,
  })
}

const FACET_LIMIT = 50

/**
 * The facet-local hint set of one column's value search: the term resolved
 * against the opened column's formatter only. Returned under the dedicated
 * `facetSearchHints` key so it can never replace the global `searchHints`.
 */
function facetLocalHints(
  column: AuditGridColumnId,
  search: string,
  locale: Locale,
  t: TFunction,
): Pick<AuditFacetParams, 'facetSearchHints'> {
  const hints = deriveAuditColumnSearch(column, search, locale, t)
  return Object.keys(hints).length > 0 ? { facetSearchHints: hints } : {}
}

/**
 * The open menu owns UI state only; the complete authorized scope plus the
 * active column and its value search are the query cache key. A menu opened
 * mid-request cancels its own call through TanStack Query.
 */
export function useAuditFacets(scope: AuditScope, enabled: boolean, locale: Locale, t: TFunction) {
  const [activeColumn, setActiveColumn] = useState<AuditGridColumnId | null>(null)
  const [search, setSearch] = useState('')

  const facetParams = useMemo<AuditFacetParams>(
    () => ({
      // The global `searchHints` (whole-table term) come from the scope owner.
      ...scopeParams(scope, locale, t),
      facetColumn: activeColumn ?? 'action',
      ...(search ? { facetSearch: search } : {}),
      // A facet's value search is local to its column: it resolves the term
      // against that column's formatter only and travels as its own
      // `facetSearchHints`, never widened into the global `searchHints`.
      ...(activeColumn && search ? facetLocalHints(activeColumn, search, locale, t) : {}),
      facetLimit: FACET_LIMIT,
    }),
    [activeColumn, locale, scope, search, t],
  )

  const query = useInfiniteQuery({
    ...auditFacetsInfiniteOptions(facetParams),
    enabled: enabled && Boolean(activeColumn),
    staleTime: 0,
  })

  const values = useMemo(() => query.data?.pages.flatMap((page) => page.values) ?? [], [query.data])

  const page = useCallback(
    (columnId: string): GridFacetPage | undefined => {
      if (!activeColumn || columnId !== activeColumn) return undefined
      return {
        options: auditFacetOptions(activeColumn, values, t, locale),
        hasMore: query.hasNextPage,
        pending: query.isFetching,
        error: query.isError,
      }
    },
    [activeColumn, locale, query.hasNextPage, query.isError, query.isFetching, t, values],
  )
  const { fetchNextPage, hasNextPage, isFetching } = query

  const serverFacets = useMemo<GridServerFacets>(
    () =>
      buildGridServerFacets({
        page,
        setActiveColumn: (columnId) => setActiveColumn(columnId as AuditGridColumnId),
        clearSearch: () => setSearch(''),
        setSearch,
        hasNextPage,
        isFetching,
        fetchNextPage: () => {
          void fetchNextPage()
        },
      }),
    [page, hasNextPage, isFetching, fetchNextPage],
  )
  return { serverFacets }
}

/**
 * The authoritative labels of the *selected* values. Only the actor label is
 * server-owned: a principal id is not derivable on the client, so it is
 * resolved by key through the facet owner, and a selection made on another page
 * or reloaded from the URL still shows its server label instead of falling back
 * to the raw id. Every other column already has a canonical local formatter
 * (`auditFacetLabel`), so it is never looked up — the endpoint takes one
 * `facetColumn` projection, and enumerating enum/date rows for a label the grid
 * can render itself would be both wrong across mixed columns and needless.
 * `resolveLabel` returns `undefined` for an actor the server has not answered
 * yet, which the caller renders as a pending chip rather than a humanized id.
 * The null actor bucket has no id to resolve and keeps the shared null token.
 */
export function useAuditSelectedValues(
  scope: AuditScope,
  filters: GridState['filters'],
  locale: Locale,
  t: TFunction,
) {
  const selectedValues = useMemo<AuditSelectedValue[]>(() => {
    const selected: AuditSelectedValue[] = []
    const actorKeys = filters.actor
    if (!actorKeys || actorKeys.length === 0) return selected
    for (const key of actorKeys) {
      const value = gridFacetValue(key)
      if (typeof value === 'string' && value !== '') {
        selected.push({ column: 'actor', value, label: '' })
      }
    }
    return selected
  }, [filters])

  const query = useQuery({
    queryKey: ['auditSelectedValues', scope, locale, selectedValues, 'actor', t],
    queryFn: ({ signal }) =>
      auditApi.facets(
        {
          ...scopeParams(scope, locale, t),
          facetColumn: 'actor',
          selectedValues,
          facetLimit: selectedValues.length,
        },
        signal,
      ),
    enabled: selectedValues.length > 0,
    staleTime: 0,
    placeholderData: keepPreviousData,
  })

  const byKey = useMemo(() => {
    const map = new Map<string, string>()
    for (const row of query.data?.values ?? []) {
      if (row.value !== null && row.value !== undefined) {
        map.set(gridFacetKey(row.value), row.label)
      }
    }
    return map
  }, [query.data])

  const resolveLabel = useCallback(
    (_columnId: AuditGridColumnId, key: string) => byKey.get(key),
    [byKey],
  )
  return { pending: query.isFetching && byKey.size === 0, resolveLabel, selectedValues }
}
