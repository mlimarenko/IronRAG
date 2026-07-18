import { keepPreviousData, useQuery, useQueryClient } from '@tanstack/react-query'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { TFunction } from 'i18next'
import { toast } from 'sonner'

import {
  documentsApi,
  queries,
  type DocumentListPageResponse,
  type DocumentListStatusFilter,
} from '@/shared/api'
import type { DocumentItem, Library, Workspace } from '@/shared/types'
import { mapListItem } from '@/features/documents/model/documentAdapter'

import {
  extractWebIngestPolicy,
  getErrorMessage,
  getFilteredTotal,
  LIST_POLL_INTERVAL_MS,
  parseCost,
  splitSortValue,
  type DocumentsStatusBucket,
  type PageSizeOption,
  type SortValue,
  type UpdateSearchParamState,
} from './documentsPageState'
import {
  buildPaginationState,
  useCursorStack,
  useDebouncedSearch,
  useListPollGrace,
} from './documentsQueryState'
import { useInspectorQueries } from './useInspectorQueries'

type UseDocumentsQueriesInput = {
  activeLibrary: Library | null
  activeWorkspace: Workspace | null
  pageSize: PageSizeOption
  searchQuery: string
  selectedDocumentId: string | null
  sortValue: SortValue
  statusBackendFilter: DocumentListStatusFilter[]
  statusBucket: DocumentsStatusBucket
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

export function useDocumentsQueries({
  activeLibrary,
  activeWorkspace,
  pageSize,
  searchQuery,
  selectedDocumentId,
  sortValue,
  statusBackendFilter,
  statusBucket,
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
    pageSize,
    sortValue,
    statusBucket,
  })
  const { activateListPollGrace, shouldPoll } = useListPollGrace()

  const listQueryRefetchRef = useRef<(() => Promise<unknown>) | null>(null)
  const aggregatesQueryRefetchRef = useRef<(() => Promise<unknown>) | null>(null)
  const activeCursor = cursorStack[cursorStack.length - 1] ?? null
  const { sortBy, sortOrder } = splitSortValue(sortValue)
  const listQueryParameters = useMemo(
    () => ({
      ...(activeLibraryId ? { libraryId: activeLibraryId } : {}),
      limit: pageSize,
      ...(activeCursor ? { cursor: activeCursor } : {}),
      ...(debouncedSearch ? { search: debouncedSearch } : {}),
      sortBy,
      sortOrder,
      ...(statusBackendFilter.length > 0 ? { status: statusBackendFilter.join(',') } : {}),
    }),
    [
      activeCursor,
      activeLibraryId,
      debouncedSearch,
      pageSize,
      sortBy,
      sortOrder,
      statusBackendFilter,
    ],
  )
  const aggregateQueryParameters = useMemo(
    () => ({
      ...(activeLibraryId ? { libraryId: activeLibraryId } : {}),
      limit: 1,
      ...(debouncedSearch ? { search: debouncedSearch } : {}),
      sortBy,
      sortOrder,
      includeTotal: true,
      ...(statusBackendFilter.length > 0 ? { status: statusBackendFilter.join(',') } : {}),
    }),
    [activeLibraryId, debouncedSearch, sortBy, sortOrder, statusBackendFilter],
  )
  const listQuery = useQuery({
    ...queries.listContentDocumentsOptions({ query: listQueryParameters }),
    enabled: !!activeLibraryId,
    placeholderData: keepPreviousData,
    staleTime: 0,
    refetchInterval: (q) => {
      const hasInFlight = (q.state.data?.items ?? []).some(
        (doc) => doc.status === 'queued' || doc.status === 'processing',
      )
      return hasInFlight || shouldPoll ? LIST_POLL_INTERVAL_MS : false
    },
    refetchIntervalInBackground: false,
  })
  const aggregatesQuery = useQuery({
    ...queries.listContentDocumentsOptions({ query: aggregateQueryParameters }),
    enabled: !!activeLibraryId,
    staleTime: 5_000,
  })
  useEffect(() => {
    listQueryRefetchRef.current = listQuery.refetch
    aggregatesQueryRefetchRef.current = aggregatesQuery.refetch
  }, [aggregatesQuery.refetch, listQuery.refetch])

  const loadFirstPage = useCallback(async () => {
    setCursorStack([null])
    await Promise.all([listQueryRefetchRef.current?.(), aggregatesQueryRefetchRef.current?.()])
  }, [setCursorStack])

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
      query: { libraryId: activeLibraryId ?? '' },
    }),
    enabled: !!activeLibraryId,
    staleTime: 30_000,
  })
  const workspaceCostQuery = useQuery({
    ...queries.getWorkspaceCostSummaryOptions({
      query: { workspaceId: activeWorkspace?.id ?? '' },
    }),
    enabled: !!activeWorkspace?.id,
    staleTime: 30_000,
  })
  const webRunsQuery = useQuery({
    queryKey: ['webRuns', activeLibraryId, activeLibrary],
    queryFn: () =>
      activeLibrary ? documentsApi.listWebRuns(activeLibrary.id) : Promise.resolve([]),
    enabled: !!activeLibraryId,
    staleTime: 0,
  })
  const { refetch: refetchWebRuns } = webRunsQuery
  const refreshWebRuns = useCallback(async () => {
    await refetchWebRuns()
  }, [refetchWebRuns])

  const items = useMemo(
    () => (listQuery.data?.items ?? []).map((raw) => mapListItem(raw, t)),
    [listQuery.data, t],
  )

  // Deep-link fallback: when the URL carries a `documentId` that is not on
  // the currently loaded list page (e.g. the Graph view's "open document"
  // action on a document buried thousands of rows deep), resolve that one
  // document through the *same* list endpoint — filtered by id — so it runs
  // through the canonical `mapListItem` derivation. This is the single
  // source of truth for a `DocumentItem`: no parallel detail→item mapper
  // that would have to re-derive the server-side `status`/`readiness`
  // buckets client-side.
  const isSelectedOnPage =
    !!selectedDocumentId && items.some((doc) => doc.id === selectedDocumentId)
  const needsSelectedFallback = !!activeLibraryId && !!selectedDocumentId && !isSelectedOnPage
  const selectedFallbackQuery = useQuery({
    ...queries.listContentDocumentsOptions({
      query: {
        ...(activeLibraryId ? { libraryId: activeLibraryId } : {}),
        ids: selectedDocumentId ?? '',
        limit: 1,
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
    return raw ? mapListItem(raw, t) : null
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
  const filteredTotal = getFilteredTotal(statusBucket, statusCounts, totalCount)
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
    libraryCost: parseCost(libraryCostQuery.data?.totalCost) ?? 0,
    libraryPolicyQuery,
    loadedWebIngestPolicy,
    loadError: listQuery.error ? errorMessage(listQuery.error, t('documents.failedToLoad')) : null,
    loadFirstPage,
    pagination,
    refreshWebRuns,
    selectDoc,
    selectedDoc,
    sortBy,
    sortOrder,
    statusCounts,
    totalCount,
    updateDocumentHintLocally,
    webRuns: webRunsQuery.data ?? [],
    webRunsRefreshing: webRunsQuery.isFetching,
    workspaceCost: parseCost(workspaceCostQuery.data?.totalCost) ?? 0,
  }
}
