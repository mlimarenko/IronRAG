import { useInfiniteQuery } from '@tanstack/react-query'
import { useCallback, useEffect, useMemo, useState } from 'react'
import type { TFunction } from 'i18next'

import {
  documentFacetsInfiniteOptions,
  type DocumentColumnFilters,
  type DocumentFacetParams,
  type DocumentFacetColumn,
  type DocumentIndexingFilter,
  type DocumentListStatusFilter,
} from '@/shared/api'
import { buildGridServerFacets, type GridFacetPage } from '@/shared/data-grid'
import type { DocumentSearchRanges } from '@/shared/api/generated'
import type { Locale } from '@/shared/types'
import { documentFacetOptions } from '@/features/documents/model/documentsGrid'
import {
  deriveDocumentSearch,
  type DocumentSearchDerivation,
} from '@/features/documents/model/documentsSearch'
import { useDebouncedSearch } from './documentsQueryState'

export interface DocumentFacetScope {
  columnFilters: DocumentColumnFilters | undefined
  search: string
  searchStatus: DocumentListStatusFilter[]
  normalizedSearch: string | null
  searchRanges: DocumentSearchRanges | null
  status: DocumentListStatusFilter[] | undefined
  indexing: DocumentIndexingFilter | undefined
  ids: string[] | undefined
  snapshotAt: string | null
}

function facetSearchParameters(
  column: DocumentFacetColumn | null,
  derivation: DocumentSearchDerivation,
) {
  const parameters: Pick<
    DocumentFacetParams,
    'facetSearch' | 'facetSearchRanges' | 'facetSearchStatus'
  > = {}
  if (!derivation.search) return parameters
  parameters.facetSearch = derivation.search
  switch (column) {
    case 'cost':
      parameters.facetSearch = derivation.normalizedSearch ?? derivation.search
      break
    case 'file_size':
      if (derivation.searchRanges?.fileSize)
        parameters.facetSearchRanges = { fileSize: derivation.searchRanges.fileSize }
      break
    case 'pipeline_time':
      if (derivation.searchRanges?.pipelineTime)
        parameters.facetSearchRanges = { pipelineTime: derivation.searchRanges.pipelineTime }
      break
    case 'uploaded_at':
    case 'finished_at':
      if (derivation.searchRanges?.date)
        parameters.facetSearchRanges = { date: derivation.searchRanges.date }
      break
    case 'status':
      if (derivation.searchStatus.length) parameters.facetSearchStatus = derivation.searchStatus
      break
  }
  return parameters
}

/** The open menu owns UI state only; the complete authorized scope is the query cache key. */
export function useDocumentFacets({
  libraryId,
  enabled,
  locale,
  onSnapshot,
  scope,
  t,
}: Readonly<{
  libraryId: string | null
  enabled: boolean
  locale: Locale
  onSnapshot: (snapshotAt: string) => void
  scope: DocumentFacetScope
  t: TFunction
}>) {
  const [activeColumn, setActiveColumn] = useState<DocumentFacetColumn | null>(null)
  const [search, setSearch] = useState('')
  const debouncedSearch = useDebouncedSearch(search)
  const facetSearch = search ? debouncedSearch : ''
  const facetSearchDerivation = useMemo(
    () => deriveDocumentSearch(facetSearch, locale, t),
    [facetSearch, locale, t],
  )
  const query = useInfiniteQuery({
    ...documentFacetsInfiniteOptions({
      libraryId: libraryId ?? '',
      facetColumn: activeColumn ?? 'file_name',
      facetLimit: 50,
      ...(scope.columnFilters ? { columnFilters: scope.columnFilters } : {}),
      ...(scope.search ? { search: scope.search } : {}),
      ...(scope.searchStatus.length ? { searchStatus: scope.searchStatus } : {}),
      ...(scope.normalizedSearch ? { normalizedSearch: scope.normalizedSearch } : {}),
      ...(scope.searchRanges ? { searchRanges: scope.searchRanges } : {}),
      ...(scope.status?.length ? { status: scope.status } : {}),
      ...(scope.indexing ? { indexing: scope.indexing } : {}),
      ...(scope.ids !== undefined ? { ids: scope.ids } : {}),
      ...(scope.snapshotAt ? { snapshotAt: scope.snapshotAt } : {}),
      ...facetSearchParameters(activeColumn, facetSearchDerivation),
    }),
    enabled: enabled && Boolean(libraryId && activeColumn),
    staleTime: 0,
    refetchInterval: (q) =>
      q.state.data?.pages.some((page) => page.costRollupFreshness === 'reconciling')
        ? 10_000
        : false,
  })
  const snapshot = query.data?.pages[0]?.snapshotAt
  useEffect(() => {
    if (snapshot && !scope.snapshotAt) onSnapshot(snapshot)
  }, [onSnapshot, scope.snapshotAt, snapshot])

  const values = useMemo(() => query.data?.pages.flatMap((page) => page.values) ?? [], [query.data])
  // A cost-dependent facet meets a reconciling rollup as an explicit pending
  // response: empty `values` with a `reconciling` marker. The menu must keep
  // selected chips and say "loading", never "no values", and never treat it as
  // the finished empty set.
  const facetPending = query.data?.pages.some((page) => page.scopePending) ?? false
  const page = useCallback(
    (columnId: string): GridFacetPage | undefined => {
      if (columnId !== activeColumn) return undefined
      return {
        options: documentFacetOptions(activeColumn, values, t, locale),
        hasMore: query.hasNextPage && !facetPending,
        pending: query.isFetching || facetSearch !== search || facetPending,
        error: query.isError,
      }
    },
    [
      activeColumn,
      facetPending,
      facetSearch,
      locale,
      query.hasNextPage,
      query.isError,
      query.isFetching,
      search,
      t,
      values,
    ],
  )
  const serverFacets = buildGridServerFacets({
    page,
    setActiveColumn: (columnId) => setActiveColumn(columnId as DocumentFacetColumn),
    clearSearch: () => setSearch(''),
    setSearch,
    hasNextPage: query.hasNextPage && !facetPending,
    isFetching: query.isFetching,
    fetchNextPage: () => void query.fetchNextPage(),
  })
  return { serverFacets }
}
