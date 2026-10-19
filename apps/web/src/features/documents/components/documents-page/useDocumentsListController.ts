import { useCallback, useEffect, useRef, useState } from 'react'
import type { Dispatch, SetStateAction } from 'react'
import type { TFunction } from 'i18next'
import { toast } from 'sonner'

import {
  ASYNC_OPERATION_TERMINAL_STATES,
  documentsApi,
  type DocumentColumnFilters,
  type DocumentListSortKey,
  type DocumentListSortOrder,
  type DocumentListStatusFilter,
} from '@/shared/api'
import type { DocumentItem, Library } from '@/shared/types'
import type { DocumentSearchRanges } from '@/shared/api/generated'

import {
  selectAllMatchingAvailable,
  splitSortValue,
  type BulkRerunState,
  type DocumentsListFilter,
  type DocumentsTableState,
  type PageSizeOption,
  type SortValue,
  type UpdateSearchParamState,
} from './documentsPageState'
import { useBulkRerunProgressQuery } from './useBulkRerunProgressQuery'

type DocumentsListControllerInput = {
  activeLibrary: Library
  activateListPollGrace: () => void
  errorMessage: (error: unknown, fallback: string) => string
  filteredTotal: number | null
  /**
   * Whether the current filter has another cursor page. The indexing buckets
   * carry no total, so this is the only signal that "select all matching" has
   * anything more to collect.
   */
  hasMorePages: boolean
  items: DocumentItem[]
  loadFirstPage: () => Promise<void>
  pageSize: PageSizeOption
  setTableState: Dispatch<SetStateAction<DocumentsTableState>>
  sortBy: DocumentListSortKey
  sortOrder: DocumentListSortOrder
  sortValue: SortValue
  listFilter: DocumentsListFilter
  /** Everything the list is filtering by, repeated for "select all matching". */
  columnFilters: DocumentColumnFilters | undefined
  search: string
  searchStatus: DocumentListStatusFilter[]
  normalizedSearch: string | null
  searchRanges: DocumentSearchRanges | null
  snapshotAt: string | null
  t: TFunction
  updateSearchParamState: UpdateSearchParamState
}

type DocumentListRequest = Parameters<typeof documentsApi.list>[0]

/**
 * The one request the "select all matching" cursor walk builds. Every optional
 * scope is attached only when it is set, so an absent filter stays absent on
 * the wire rather than becoming an empty, fail-closed value.
 */
function buildSelectionRequest(input: {
  libraryId: string
  limit: number
  sortBy: DocumentListSortKey
  sortOrder: DocumentListSortOrder
  cursor: string | null | undefined
  search: string
  searchStatus: DocumentListStatusFilter[]
  normalizedSearch: string | null
  searchRanges: DocumentSearchRanges | null
  snapshotAt: string | null
  columnFilters: DocumentColumnFilters | undefined
  status: DocumentListStatusFilter[] | undefined
  indexing: DocumentsListFilter['indexing']
}): DocumentListRequest {
  const request: DocumentListRequest = {
    libraryId: input.libraryId,
    limit: input.limit,
    sortBy: input.sortBy,
    sortOrder: input.sortOrder,
    includeTotal: false,
  }
  if (input.cursor) request.cursor = input.cursor
  if (input.search) request.search = input.search
  if (input.searchStatus.length > 0) request.searchStatus = input.searchStatus
  if (input.normalizedSearch) request.normalizedSearch = input.normalizedSearch
  if (input.searchRanges) request.searchRanges = input.searchRanges
  if (input.snapshotAt) request.snapshotAt = input.snapshotAt
  if (input.columnFilters) request.columnFilters = input.columnFilters
  if (input.status) request.status = input.status
  if (input.indexing) request.indexing = input.indexing
  return request
}

export function useDocumentsListController({
  activeLibrary,
  activateListPollGrace,
  errorMessage,
  filteredTotal,
  hasMorePages,
  items,
  loadFirstPage,
  pageSize,
  setTableState,
  sortBy,
  sortOrder,
  sortValue,
  listFilter,
  columnFilters,
  search,
  searchStatus,
  normalizedSearch,
  searchRanges,
  snapshotAt,
  t,
  updateSearchParamState,
}: DocumentsListControllerInput) {
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set())
  const [expandingSelection, setExpandingSelection] = useState(false)
  const [bulkRerun, setBulkRerun] = useState<BulkRerunState | null>(null)
  const terminalToastFiredRef = useRef(false)
  const bulkQuery = useBulkRerunProgressQuery(bulkRerun)

  const clearSelection = useCallback(() => {
    setSelectedIds(new Set())
    setExpandingSelection(false)
  }, [])
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && selectedIds.size > 0) clearSelection()
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [clearSelection, selectedIds.size])

  const toggleSelection = useCallback((id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }, [])
  /**
   * The header checkbox acts on exactly the rows on screen, in one action.
   * Selection is an id set, so ids chosen on another page are untouched.
   */
  const toggleAllVisible = useCallback((checked: boolean, rows: DocumentItem[]) => {
    setSelectedIds((prev) => {
      const next = new Set(prev)
      for (const document of rows) {
        if (checked) next.add(document.id)
        else next.delete(document.id)
      }
      return next
    })
  }, [])
  const onSortChange = useCallback(
    (value: SortValue) =>
      updateSearchParamState({
        sort: value === 'uploaded_at:desc' ? null : value,
        documentId: null,
      }),
    [updateSearchParamState],
  )
  const toggleSortDirection = useCallback(
    (target: DocumentListSortKey) => {
      if (sortBy !== target) {
        onSortChange(`${target}:${sortOrder}`)
        return
      }
      onSortChange(`${target}:${sortOrder === 'asc' ? 'desc' : 'asc'}`)
    },
    [onSortChange, sortBy, sortOrder],
  )
  const toggleDetailColumns = useCallback(() => {
    setTableState((prev) => ({ ...prev, showDetailColumns: !prev.showDetailColumns }))
  }, [setTableState])

  /**
   * Walks every page the current filter, search and sort produce, not only the
   * loaded page: the same cursor call the list makes, carrying the same column
   * filters, typed search and status scope.
   */
  const selectAllMatching = useCallback(async () => {
    if (expandingSelection) return
    setExpandingSelection(true)
    try {
      const split = splitSortValue(sortValue)
      const collected = new Set(selectedIds)
      let cursor: string | null | undefined
      let selectionSnapshot = snapshotAt
      while (collected.size < 100_000) {
        const page = await documentsApi.list(
          buildSelectionRequest({
            libraryId: activeLibrary.id,
            limit: pageSize,
            sortBy: split.sortBy,
            sortOrder: split.sortOrder,
            cursor,
            search,
            searchStatus,
            normalizedSearch,
            searchRanges,
            snapshotAt: selectionSnapshot,
            columnFilters,
            status: listFilter.status,
            indexing: listFilter.indexing,
          }),
        )
        for (const row of page.items) collected.add(row.id)
        selectionSnapshot ??= page.snapshotAt
        if (!page.nextCursor) break
        cursor = page.nextCursor
      }
      setSelectedIds(collected)
    } catch (err) {
      toast.error(errorMessage(err, t('documents.failedToLoad')))
    } finally {
      setExpandingSelection(false)
    }
  }, [
    activeLibrary.id,
    columnFilters,
    errorMessage,
    expandingSelection,
    listFilter.indexing,
    listFilter.status,
    normalizedSearch,
    pageSize,
    search,
    searchStatus,
    searchRanges,
    snapshotAt,
    selectedIds,
    sortValue,
    t,
  ])

  const startBulkRerun = useCallback(
    (kind: BulkRerunState['kind'], operationId: string, total: number) => {
      activateListPollGrace()
      clearSelection()
      terminalToastFiredRef.current = false
      setBulkRerun({
        kind,
        operationId,
        total,
        completed: 0,
        failed: 0,
        inFlight: total,
        status: 'processing',
      })
    },
    [activateListPollGrace, clearSelection],
  )
  const handleBulkDelete = useCallback(async () => {
    if (!confirm(t('documents.confirmBulkDelete', { count: selectedIds.size }))) return
    const ids = Array.from(selectedIds)
    if (ids.length === 0) return
    try {
      const accepted = await documentsApi.batchDelete(ids)
      startBulkRerun('delete', accepted.batchOperationId, accepted.total)
    } catch (err) {
      toast.error(errorMessage(err, t('documents.bulkDeleteFailed')))
    }
  }, [errorMessage, selectedIds, startBulkRerun, t])
  const handleBulkCancel = useCallback(async () => {
    try {
      await documentsApi.batchCancel(Array.from(selectedIds))
      toast.success(t('documents.bulkCancelSuccess', { count: selectedIds.size }))
      clearSelection()
      await loadFirstPage()
    } catch {
      toast.error(t('documents.bulkCancelFailed'))
    }
  }, [clearSelection, loadFirstPage, selectedIds, t])
  const handleBulkReprocess = useCallback(async () => {
    const ids = Array.from(selectedIds)
    if (ids.length === 0) return
    try {
      const accepted = await documentsApi.batchReprocess(ids)
      startBulkRerun('reprocess', accepted.batchOperationId, accepted.total)
    } catch {
      toast.error(t('documents.bulkReprocessFailed'))
    }
  }, [selectedIds, startBulkRerun, t])
  const reprocessDocument = useCallback(
    async (documentId: string) => {
      try {
        await documentsApi.reprocess(documentId)
        activateListPollGrace()
        await loadFirstPage()
      } catch (err) {
        toast.error(errorMessage(err, t('documents.reprocessFailed')))
      }
    },
    [activateListPollGrace, errorMessage, loadFirstPage, t],
  )

  useEffect(() => {
    const detail = bulkQuery.operationQuery.data
    if (!detail || !bulkRerun) return
    if (!ASYNC_OPERATION_TERMINAL_STATES.has(detail.status) || terminalToastFiredRef.current) return
    terminalToastFiredRef.current = true
    const isDelete = bulkRerun.kind === 'delete'
    if (detail.status === 'ready') {
      toast.success(
        isDelete
          ? t('documents.bulkDeleteSuccess', { count: detail.progress.completed })
          : t('documents.bulkReprocessSuccess', { count: detail.progress.completed }),
      )
    } else if (detail.progress.completed > 0) {
      toast.warning(
        isDelete
          ? t('documents.bulkDeletePartial', {
              ok: detail.progress.completed,
              failed: detail.progress.failed,
            })
          : t('documents.bulkReprocessPartial', {
              ok: detail.progress.completed,
              failed: detail.progress.failed,
            }),
      )
    } else {
      toast.error(
        isDelete
          ? t('documents.bulkDeleteAllFailed', { count: detail.progress.failed })
          : t('documents.bulkReprocessAllFailed', { count: detail.progress.failed }),
      )
    }
    void loadFirstPage()
    const timeoutId = window.setTimeout(() => setBulkRerun(null), 4000)
    return () => window.clearTimeout(timeoutId)
  }, [bulkQuery.operationQuery.data, bulkRerun, loadFirstPage, t])

  return {
    bulkProgress: bulkQuery.progress,
    clearSelection,
    expandingSelection,
    handleBulkCancel,
    handleBulkDelete,
    handleBulkReprocess,
    reprocessDocument,
    selectAllMatching,
    selectedIds,
    setBulkRerun,
    setSelectedIds,
    showSelectAllMatching: selectAllMatchingAvailable({
      allVisibleSelected: items.length > 0 && items.every((doc) => selectedIds.has(doc.id)),
      filteredTotal,
      hasMorePages,
      itemCount: items.length,
      selectedCount: selectedIds.size,
    }),
    toggleAllVisible,
    toggleDetailColumns,
    toggleSelection,
    toggleSortDirection,
  }
}
