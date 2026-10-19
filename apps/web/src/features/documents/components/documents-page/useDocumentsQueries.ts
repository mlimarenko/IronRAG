import { keepPreviousData, useQuery, useQueryClient } from '@tanstack/react-query'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { TFunction } from 'i18next'
import { toast } from 'sonner'

import {
  queries,
  documentListQuery,
  type DocumentColumnFilters,
  type DocumentListPageResponse,
} from '@/shared/api'
import type { ListContentDocumentsData } from '@/shared/api/generated'
import type { DocumentItem, Library, Locale, Workspace } from '@/shared/types'
import { mapListItem } from '@/features/documents/model/documentAdapter'
import {
  deriveDocumentSearch,
  type DocumentSearchDerivation,
} from '@/features/documents/model/documentsSearch'

import {
  extractWebIngestPolicy,
  getErrorMessage,
  LIST_POLL_INTERVAL_MS,
  parseCost,
  splitSortValue,
  type DocumentsListFilter,
  type DocumentsStatusBucket,
  type PageSizeOption,
  type SortValue,
  type UpdateSearchParamState,
} from './documentsPageState'
import {
  buildPaginationState,
  isInvalidCursorError,
  useCursorStack,
  useDebouncedSearch,
  useListPollGrace,
} from './documentsQueryState'
import { useDocumentFacets } from './useDocumentFacets'
import { useInspectorQueries } from './useInspectorQueries'

type DocumentListQuery = NonNullable<ListContentDocumentsData['query']>

/// Re-reads a cost summary only while the backend reports it reconciling. Long
/// enough to be gentle on the API, short enough that the cell stops saying
/// "updating" shortly after the worker repairs the generation.
const COST_RECONCILE_POLL_INTERVAL_MS = 10_000

type UseDocumentsQueriesInput = {
  activeLibrary: Library | null
  activeWorkspace: Workspace | null
  locale: Locale
  pageSize: PageSizeOption
  searchQuery: string
  selectedDocumentId: string | null
  sortValue: SortValue
  listFilter: DocumentsListFilter
  statusBucket: DocumentsStatusBucket
  columnFilters: DocumentColumnFilters | undefined
  filtersKey: string
  snapshotAt: string | null
  t: TFunction
  updateSearchParamState: UpdateSearchParamState
}

function isListContentDocumentsQueryKey(queryKey: readonly unknown[]): boolean {
  const [params] = queryKey
  const candidate = params as { _id?: unknown; _infinite?: unknown } | null
  return (
    !!candidate &&
    typeof candidate === 'object' &&
    candidate._id === 'listContentDocuments' &&
    candidate._infinite !== true
  )
}

export interface DocumentListQueryInput {
  cursor: string | null
  pageSize: number
  sortValue: SortValue
  derivation: DocumentSearchDerivation
  columnFilters: DocumentColumnFilters | undefined
  listFilter: DocumentsListFilter
  snapshotAt: string | null
}

/**
 * The exact query object the list transport is called with. One pure builder so
 * a test can assert the request shape — including that a cost header change
 * arrives as `sortBy: 'cost'` on every page, not as a page-scoped reorder — and
 * so the hook cannot drift from what it claims to send.
 */
export function buildDocumentListQuery({
  cursor,
  pageSize,
  sortValue,
  derivation,
  columnFilters,
  listFilter,
  snapshotAt,
}: DocumentListQueryInput): DocumentListQuery {
  const { sortBy, sortOrder } = splitSortValue(sortValue)
  return documentListQuery({
    limit: pageSize,
    sortBy,
    sortOrder,
    includeTotal: true,
    ...(cursor ? { cursor } : {}),
    ...(derivation.search ? { search: derivation.search } : {}),
    searchStatus: derivation.searchStatus,
    ...(derivation.normalizedSearch ? { normalizedSearch: derivation.normalizedSearch } : {}),
    ...(derivation.searchRanges ? { searchRanges: derivation.searchRanges } : {}),
    ...(columnFilters ? { columnFilters } : {}),
    ...(snapshotAt ? { snapshotAt } : {}),
    ...listFilter,
  })
}

export function useDocumentsQueries({
  activeLibrary,
  activeWorkspace,
  locale,
  pageSize,
  searchQuery,
  selectedDocumentId,
  sortValue,
  listFilter,
  statusBucket,
  columnFilters,
  filtersKey,
  snapshotAt,
  t,
  updateSearchParamState,
}: UseDocumentsQueriesInput) {
  const queryClient = useQueryClient()
  const activeLibraryId = activeLibrary?.id ?? null
  const [selectedDocSnapshot, setSelectedDocSnapshot] = useState<DocumentItem | null>(null)
  const debouncedSearch = useDebouncedSearch(searchQuery)
  const { cursorStack, setCursorStack } = useCursorStack({
    activeLibraryId,
    debouncedSearch,
    filtersKey,
    pageSize,
    snapshotAt,
    sortValue,
    statusBucket,
  })
  const { activateListPollGrace, shouldPoll } = useListPollGrace()

  const listQueryRefetchRef = useRef<(() => Promise<unknown>) | null>(null)
  const aggregatesQueryRefetchRef = useRef<(() => Promise<unknown>) | null>(null)
  const activeCursor = cursorStack[cursorStack.length - 1] ?? null
  const { sortBy, sortOrder } = splitSortValue(sortValue)
  // One derivation owns the literal search, the derived-status buckets whose
  // localized label matches it and the Intl-normalized spelling. No caller
  // re-derives them, so list and facet requests always agree.
  const derivation = useMemo(
    () => deriveDocumentSearch(debouncedSearch, locale, t),
    [debouncedSearch, locale, t],
  )

  /**
   * Adopt the server snapshot once and keep it for the whole cursor chain. A
   * duration facet selected later therefore counts against the same instant as
   * the page it opens; a manual refresh or a scope change starts a new clock.
   */
  const adoptSnapshot = useCallback(
    (snapshot: string | null) => {
      if (!snapshot || searchQuery !== debouncedSearch) return
      if (snapshotAt === null) {
        updateSearchParamState({ snapshotAt: snapshot })
      }
    },
    [debouncedSearch, searchQuery, snapshotAt, updateSearchParamState],
  )

  const listQueryParameters = useMemo(
    () =>
      buildDocumentListQuery({
        cursor: activeCursor,
        pageSize,
        sortValue,
        derivation,
        columnFilters,
        listFilter,
        snapshotAt,
      }),
    [activeCursor, columnFilters, derivation, listFilter, pageSize, snapshotAt, sortValue],
  )
  // Deliberately without the bucket filter: `totalCount` and `statusCounts`
  // are the per-bucket chip numbers, so they have to be the whole library
  // within the current grid filters and derived search. The exclusive bucket
  // (status OR indexing) is self-excluded: these are counts for switching the
  // bucket, not an aggregate of the current indexing subset.
  const aggregateQueryParameters = useMemo(
    () =>
      buildDocumentListQuery({
        cursor: null,
        pageSize: 1,
        derivation,
        columnFilters,
        snapshotAt,
        sortValue,
        listFilter: {},
      }),
    [columnFilters, derivation, snapshotAt, sortValue],
  )
  const listQuery = useQuery({
    ...queries.listContentDocumentsOptions({
      path: { libraryId: activeLibraryId ?? '' },
      query: listQueryParameters,
    }),
    enabled: !!activeLibraryId,
    placeholderData: keepPreviousData,
    staleTime: 0,
    refetchInterval: (q) => {
      const hasInFlight = (q.state.data?.items ?? []).some(
        (doc) => doc.status === 'queued' || doc.status === 'processing',
      )
      if (hasInFlight || shouldPoll) return LIST_POLL_INTERVAL_MS
      return q.state.data?.costRollupFreshness === 'reconciling'
        ? COST_RECONCILE_POLL_INTERVAL_MS
        : false
    },
    refetchIntervalInBackground: false,
  })
  useEffect(() => {
    if (!isInvalidCursorError(listQuery.error, activeCursor)) return
    setCursorStack([null])
  }, [activeCursor, listQuery.error, setCursorStack])
  // A cost-dependent read (cost sort, or a cost column filter) that meets a
  // reconciling rollup returns an explicit pending page with no continuation.
  // Drop the cost cursor stack so the very next page is a first page; the
  // pinned snapshot clock in the URL is untouched, so duration semantics keep
  // their instant. The summary poll (10s, reconciling only) then brings the
  // page back when the rollup turns current.
  // The library rollup is reconciling: a cost-dependent list read returns an
  // explicit pending page, and every cost cell is withheld. Distinct from an
  // empty match — the grid must not claim "no documents match" for that.
  const costReconciling = listQuery.data?.costRollupFreshness === 'reconciling'
  const scopePending = listQuery.data?.scopePending === true && !listQuery.isPlaceholderData
  useEffect(() => {
    if (!scopePending) return
    if (cursorStack.length > 1) setCursorStack([null])
  }, [scopePending, cursorStack.length, setCursorStack])
  useEffect(() => {
    if (!listQuery.isPlaceholderData) adoptSnapshot(listQuery.data?.snapshotAt ?? null)
  }, [adoptSnapshot, listQuery.data?.snapshotAt, listQuery.isPlaceholderData, listQueryParameters])
  const aggregatesQuery = useQuery({
    ...queries.listContentDocumentsOptions({
      path: { libraryId: activeLibraryId ?? '' },
      query: aggregateQueryParameters,
    }),
    enabled: !!activeLibraryId,
    staleTime: 5_000,
    refetchInterval: (q) =>
      q.state.data?.costRollupFreshness === 'reconciling' ? COST_RECONCILE_POLL_INTERVAL_MS : false,
  })
  useEffect(() => {
    listQueryRefetchRef.current = listQuery.refetch
    aggregatesQueryRefetchRef.current = aggregatesQuery.refetch
  }, [aggregatesQuery.refetch, listQuery.refetch])

  const loadFirstPage = useCallback(async () => {
    // A manual refresh may start a new clock; the pin exists to keep an
    // in-progress filtered chain stable, not to freeze a stale instant.
    updateSearchParamState({ snapshotAt: null })
    setCursorStack([null])
    await Promise.all([listQueryRefetchRef.current?.(), aggregatesQueryRefetchRef.current?.()])
  }, [setCursorStack, updateSearchParamState])

  const facetScope = useMemo(
    () => ({
      columnFilters,
      search: derivation.search,
      searchStatus: derivation.searchStatus,
      normalizedSearch: derivation.normalizedSearch,
      searchRanges: derivation.searchRanges,
      status: listFilter.status,
      indexing: listFilter.indexing,
      ids: undefined,
      snapshotAt,
    }),
    [columnFilters, derivation, listFilter.indexing, listFilter.status, snapshotAt],
  )
  const { serverFacets } = useDocumentFacets({
    libraryId: activeLibraryId,
    enabled: !!activeLibraryId,
    locale,
    onSnapshot: adoptSnapshot,
    scope: facetScope,
    t,
  })

  const libraryPolicyQuery = useQuery({
    ...queries.getCatalogLibraryOptions({
      path: { libraryId: activeLibraryId ?? '' },
    }),
    enabled: !!activeLibraryId,
    staleTime: 60_000,
  })
  const loadedWebIngestPolicy = useMemo(
    () => extractWebIngestPolicy(libraryPolicyQuery.data),
    [libraryPolicyQuery.data],
  )
  const libraryCostQuery = useQuery({
    ...queries.getLibraryCostSummaryOptions({
      path: { libraryId: activeLibraryId ?? '' },
    }),
    enabled: !!activeLibraryId,
    staleTime: 30_000,
    // Poll only while the owning rollup reports itself reconciling; a current
    // summary is left to its normal 30s staleness and an unmounted page stops
    // the interval automatically.
    refetchInterval: (q) =>
      q.state.data?.costRollupFreshness === 'reconciling' ? COST_RECONCILE_POLL_INTERVAL_MS : false,
  })
  const workspaceCostQuery = useQuery({
    ...queries.getWorkspaceCostSummaryOptions({
      path: { workspaceId: activeWorkspace?.id ?? '' },
    }),
    enabled: !!activeWorkspace?.id,
    staleTime: 30_000,
    refetchInterval: (q) =>
      q.state.data?.costRollupFreshness === 'reconciling' ? COST_RECONCILE_POLL_INTERVAL_MS : false,
  })
  const webRunsQuery = useQuery({
    ...queries.listContentWebIngestRunsOptions({
      query: { libraryId: activeLibraryId ?? '', limit: 50 },
    }),
    enabled: !!activeLibraryId,
    staleTime: 0,
  })
  const { refetch: refetchWebRuns } = webRunsQuery
  const refreshWebRuns = useCallback(async () => {
    await refetchWebRuns()
  }, [refetchWebRuns])

  const items = useMemo(
    () =>
      (listQuery.data?.items ?? []).map((raw) =>
        mapListItem(raw, t, listQuery.data?.costRollupFreshness),
      ),
    [listQuery.data, t],
  )

  // Deep-link fallback: when the URL carries a `documentId` that is not on
  // the currently loaded list page (e.g. the Graph view's "open document"
  // action on a document buried thousands of rows deep), resolve that one
  // document through the *same* list endpoint — filtered by id — so it runs
  // through the shared `mapListItem` derivation. This is the single
  // source of truth for a `DocumentItem`: no parallel detail→item mapper
  // that would have to re-derive the server-side `status`/`readiness`
  // buckets client-side.
  const isSelectedOnPage =
    !!selectedDocumentId && items.some((doc) => doc.id === selectedDocumentId)
  const needsSelectedFallback = !!activeLibraryId && !!selectedDocumentId && !isSelectedOnPage
  const selectedFallbackQuery = useQuery({
    ...queries.listContentDocumentsOptions({
      path: { libraryId: activeLibraryId ?? '' },
      query: {
        ids: selectedDocumentId ?? '',
        limit: 1,
        ...(snapshotAt ? { snapshotAt } : {}),
      },
    }),
    enabled: needsSelectedFallback,
    staleTime: 0,
  })
  const selectedFallbackDoc = useMemo(() => {
    if (!selectedDocumentId) return null
    const raw = (selectedFallbackQuery.data?.items ?? []).find(
      (item) => item.id === selectedDocumentId,
    )
    return raw ? mapListItem(raw, t, selectedFallbackQuery.data?.costRollupFreshness) : null
  }, [selectedDocumentId, selectedFallbackQuery.data, t])

  const selectedDoc = useMemo(() => {
    if (!selectedDocumentId) return null
    return (
      items.find((doc) => doc.id === selectedDocumentId) ??
      (selectedDocSnapshot?.id === selectedDocumentId ? selectedDocSnapshot : null) ??
      selectedFallbackDoc
    )
  }, [items, selectedDocSnapshot, selectedDocumentId, selectedFallbackDoc])
  const inspector = useInspectorQueries(selectedDoc)

  const selectDoc = useCallback(
    (doc: DocumentItem, syncQuery = true) => {
      if (syncQuery) updateSearchParamState({ documentId: doc.id })
      setSelectedDocSnapshot(doc)
    },
    [updateSearchParamState],
  )
  const updateDocumentHintLocally = useCallback(
    (documentId: string, documentHint: string | null) => {
      const normalizedDocumentHint = documentHint?.trim() || undefined
      setSelectedDocSnapshot((snapshot) =>
        snapshot?.id === documentId
          ? { ...snapshot, documentHint: normalizedDocumentHint }
          : snapshot,
      )
      queryClient.setQueriesData<DocumentListPageResponse>(
        {
          predicate: (query) => isListContentDocumentsQueryKey(query.queryKey),
        },
        (page) =>
          page
            ? {
                ...page,
                items: page.items.map((item) =>
                  item.id === documentId
                    ? { ...item, documentHint: normalizedDocumentHint ?? null }
                    : item,
                ),
              }
            : page,
      )
    },
    [queryClient],
  )
  const clearSelectedDoc = useCallback(() => {
    setSelectedDocSnapshot(null)
    updateSearchParamState({ documentId: null })
  }, [updateSearchParamState])
  const errorMessage = useCallback(
    (error: unknown, fallback: string) => getErrorMessage(error, fallback),
    [],
  )
  const fetchLibraryWebIngestPolicy = useCallback(
    async (libraryId: string) => {
      try {
        const result = await queryClient.fetchQuery({
          ...queries.getCatalogLibraryOptions({ path: { libraryId } }),
        })
        return extractWebIngestPolicy(result)
      } catch (err) {
        toast.error(errorMessage(err, t('documents.webIngestPolicyLoadFailed')))
        return null
      }
    },
    [errorMessage, queryClient, t],
  )

  const statusCounts = aggregatesQuery.data?.statusCounts ?? null
  const totalCount = aggregatesQuery.data?.totalCount ?? null
  const filteredTotal = listQuery.data?.totalCount ?? null
  const isLoading = listQuery.isLoading && !!activeLibraryId
  const pagination = buildPaginationState({
    cursorStack,
    filteredTotal,
    isLoading,
    itemCount: items.length,
    nextCursor: listQuery.data?.nextCursor ?? null,
    pageSize,
    setCursorStack,
  })

  return {
    snapshotAt: listQuery.data?.snapshotAt ?? snapshotAt,
    costReconciling,
    isScopePending: searchQuery !== debouncedSearch || listQuery.isPlaceholderData || scopePending,
    activateListPollGrace,
    aggregatesQuery,
    clearSelectedDoc,
    debouncedSearch,
    errorMessage,
    fetchLibraryWebIngestPolicy,
    fetchSelectedDetail: inspector.fetchSelectedDetail,
    filteredTotal,
    inspectorLifecycle: inspector.inspectorLifecycle,
    isLoading,
    items,
    libraryCostSummary: libraryCostQuery.data ?? null,
    libraryCost: parseCost(libraryCostQuery.data?.totalCost),
    libraryCostCoverage: libraryCostQuery.data?.costCoverage ?? null,
    libraryCostFreshness: libraryCostQuery.data?.costRollupFreshness ?? null,
    libraryUnpricedCalls: libraryCostQuery.data?.unpricedProviderCallCount ?? null,
    libraryPolicyQuery,
    loadedWebIngestPolicy,
    loadError: listQuery.error ? errorMessage(listQuery.error, t('documents.failedToLoad')) : null,
    loadFirstPage,
    pagination,
    refreshWebRuns,
    searchDerivation: derivation,
    selectDoc,
    selectedDoc,
    serverFacets,
    sortBy,
    sortOrder,
    statusCounts,
    totalCount,
    updateDocumentHintLocally,
    webRuns: webRunsQuery.data ?? [],
    webRunsRefreshing: webRunsQuery.isFetching,
    workspaceCostSummary: workspaceCostQuery.data ?? null,
    workspaceCost: parseCost(workspaceCostQuery.data?.totalCost),
    workspaceCostCoverage: workspaceCostQuery.data?.costCoverage ?? null,
    workspaceCostFreshness: workspaceCostQuery.data?.costRollupFreshness ?? null,
    workspaceUnpricedCalls: workspaceCostQuery.data?.unpricedProviderCallCount ?? null,
  }
}
