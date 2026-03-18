import { defineStore } from 'pinia'
import type {
  DocumentAccountingStatus,
  DocumentActivityStatus,
  DocumentDetail,
  DocumentFilterValues,
  DocumentMutationAccepted,
  DocumentMutationStatus,
  DocumentRow,
  DocumentStatus,
  DocumentUploadFailure,
  DocumentsSurfaceResponse,
} from 'src/models/ui/documents'
import {
  appendDocumentItem,
  deleteDocumentItem,
  fetchDocumentDetail,
  fetchDocumentsSurface,
  normalizeDocumentUploadFailure,
  replaceDocumentItem,
  reprocessDocumentItem,
  retryDocumentItem,
  uploadDocument,
} from 'src/services/api/documents'
import { useGraphStore } from './graph'
import { useShellStore } from './shell'

interface DocumentsState {
  surface: DocumentsSurfaceResponse | null
  localUploadRows: DocumentRow[]
  detail: DocumentDetail | null
  loading: boolean
  uploadLoading: boolean
  detailLoading: boolean
  mutationLoading: boolean
  error: string | null
  detailError: string | null
  searchQuery: string
  statusFilter: DocumentStatus | ''
  accountingFilter: DocumentAccountingStatus | ''
  mutationStatusFilter: DocumentMutationStatus | ''
  fileTypeFilter: string
  detailOpen: boolean
  appendDialogDocumentId: string | null
  replaceDialogDocumentId: string | null
  uploadFailures: DocumentUploadFailure[]
}

const LOCAL_UPLOAD_CONCURRENCY = 3
const FAST_REFRESH_INTERVAL_MS = 2_000
const WATCH_REFRESH_INTERVAL_MS = 4_000
const BACKLOG_REFRESH_INTERVAL_MS = 8_000
const WATCH_BACKLOG_THRESHOLD = 8
const THROTTLED_BACKLOG_THRESHOLD = 24

function hasPendingMutation(status: DocumentMutationStatus | null): boolean {
  return status === 'accepted' || status === 'reconciling'
}

function needsActivityPolling(activityStatus: DocumentActivityStatus): boolean {
  return (
    activityStatus === 'queued' ||
    activityStatus === 'active' ||
    activityStatus === 'blocked' ||
    activityStatus === 'retrying' ||
    activityStatus === 'stalled'
  )
}

function requiresWatchRefresh(activityStatus: DocumentActivityStatus): boolean {
  return (
    activityStatus === 'blocked' ||
    activityStatus === 'retrying' ||
    activityStatus === 'stalled'
  )
}

function rowNeedsPolling(row: DocumentRow): boolean {
  return needsActivityPolling(row.activityStatus) || hasPendingMutation(row.mutation.status)
}

function detailNeedsPolling(detail: DocumentDetail | null): boolean {
  if (!detail) {
    return false
  }
  return needsActivityPolling(detail.activityStatus) || hasPendingMutation(detail.mutation.status)
}

function resolveRefreshInterval(
  rows: DocumentRow[],
  detail: DocumentDetail | null,
  detailOpen: boolean,
  activeBacklogCount: number,
): number {
  const pollingRows = rows.filter(rowNeedsPolling)
  const pollDetail = detailOpen && detailNeedsPolling(detail)
  const effectiveBacklogCount = Math.max(activeBacklogCount, pollingRows.length)
  if (effectiveBacklogCount === 0 && !pollDetail) {
    return 0
  }
  const watchCadence =
    pollingRows.some((row) => requiresWatchRefresh(row.activityStatus)) ||
    (detailOpen && detail ? requiresWatchRefresh(detail.activityStatus) : false)
  if (watchCadence) {
    return FAST_REFRESH_INTERVAL_MS
  }
  const backlogForCadence = effectiveBacklogCount + (pollDetail ? 1 : 0)
  if (backlogForCadence >= THROTTLED_BACKLOG_THRESHOLD) {
    return BACKLOG_REFRESH_INTERVAL_MS
  }
  if (backlogForCadence >= WATCH_BACKLOG_THRESHOLD) {
    return WATCH_REFRESH_INTERVAL_MS
  }
  return FAST_REFRESH_INTERVAL_MS
}

function createEmptySurface(): DocumentsSurfaceResponse {
  return {
    acceptedFormats: [],
    maxSizeMb: 50,
    graphStatus: 'empty',
    graphWarning: null,
    rebuildBacklogCount: 0,
    counters: {
      queued: 0,
      processing: 0,
      ready: 0,
      readyNoGraph: 0,
      failed: 0,
    },
    filters: {
      statuses: [],
      fileTypes: [],
      accountingStatuses: [],
      mutationStatuses: [],
    },
    accounting: {
      totalEstimatedCost: null,
      settledEstimatedCost: null,
      inFlightEstimatedCost: null,
      currency: null,
      promptTokens: 0,
      completionTokens: 0,
      totalTokens: 0,
      pricedStageCount: 0,
      unpricedStageCount: 0,
      inFlightStageCount: 0,
      missingStageCount: 0,
      accountingStatus: 'unpriced',
    },
    diagnostics: {
      progress: {
        accepted: 0,
        contentExtracted: 0,
        chunked: 0,
        embedded: 0,
        extractingGraph: 0,
        graphReady: 0,
        ready: 0,
        failed: 0,
      },
      queueBacklogCount: 0,
      processingBacklogCount: 0,
      activeBacklogCount: 0,
      perStage: [],
      perFormat: [],
    },
    rows: [],
  }
}

function summarizeRows(rows: DocumentRow[]) {
  return rows.reduce<DocumentsSurfaceResponse['counters']>((accumulator, row) => {
    switch (row.status) {
      case 'queued':
        accumulator.queued += 1
        break
      case 'processing':
        accumulator.processing += 1
        break
      case 'ready':
        accumulator.ready += 1
        break
      case 'ready_no_graph':
        accumulator.readyNoGraph += 1
        break
      case 'failed':
        accumulator.failed += 1
        break
    }
    return accumulator
  }, {
    queued: 0,
    processing: 0,
    ready: 0,
    readyNoGraph: 0,
    failed: 0,
  })
}

function deriveFilters(rows: DocumentRow[]): DocumentFilterValues {
  return {
    statuses: Array.from(new Set<DocumentStatus>(rows.map((row) => row.status))),
    fileTypes: Array.from(new Set(rows.map((row) => row.fileType))).sort(),
    accountingStatuses: Array.from(
      new Set<DocumentAccountingStatus>(rows.map((row) => row.accountingStatus)),
    ).sort(),
    mutationStatuses: Array.from(
      new Set<DocumentMutationStatus>(
        rows
          .map((row) => row.mutation.status)
          .filter((value): value is DocumentMutationStatus => value !== null),
      ),
    ).sort(),
  }
}

function withRows(
  surface: DocumentsSurfaceResponse,
  rows: DocumentRow[],
): DocumentsSurfaceResponse {
  return {
    ...surface,
    rows,
    counters: summarizeRows(rows),
    filters: deriveFilters(rows),
  }
}

function stripLocalUploadRows(rows: DocumentRow[]): DocumentRow[] {
  return rows.filter((row) => !row.id.startsWith('local-upload:'))
}

function mergeLocalUploadRows(
  surface: DocumentsSurfaceResponse,
  localUploadRows: DocumentRow[],
): DocumentsSurfaceResponse {
  const baseRows = stripLocalUploadRows(surface.rows)
  if (localUploadRows.length === 0) {
    return withRows(surface, baseRows)
  }
  return withRows(surface, [...localUploadRows, ...baseRows])
}

function replaceRow(rows: DocumentRow[], oldId: string, nextRow: DocumentRow): DocumentRow[] {
  return rows.map((row) => (row.id === oldId ? nextRow : row))
}

function removeRow(rows: DocumentRow[], id: string): DocumentRow[] {
  return rows.filter((row) => row.id !== id)
}

function syncDetailContributionSummaryFromRow(
  detail: DocumentDetail | null,
  row: DocumentRow | undefined,
): DocumentDetail | null {
  if (!detail || !row) {
    return detail
  }
  return {
    ...detail,
    accountingStatus: row.accountingStatus,
    totalEstimatedCost: row.totalEstimatedCost,
    settledEstimatedCost: row.settledEstimatedCost,
    inFlightEstimatedCost: row.inFlightEstimatedCost,
    currency: row.currency,
    inFlightStageCount: row.inFlightStageCount,
    missingStageCount: row.missingStageCount,
    extractedStats: {
      ...detail.extractedStats,
      chunkCount: row.chunkCount ?? detail.extractedStats.chunkCount,
    },
    graphStats: {
      ...detail.graphStats,
      nodeCount: row.graphNodeCount ?? detail.graphStats.nodeCount,
      edgeCount: row.graphEdgeCount ?? detail.graphStats.edgeCount,
    },
  }
}

function formatFileSizeLabel(sizeBytes: number): string {
  if (sizeBytes >= 1024 * 1024) {
    return `${(sizeBytes / (1024 * 1024)).toFixed(1)} MB`
  }
  if (sizeBytes >= 1024) {
    return `${(sizeBytes / 1024).toFixed(1)} KB`
  }
  return `${String(sizeBytes)} B`
}

function inferFileType(file: File): string {
  const extension = file.name.split('.').pop()?.toLowerCase() ?? ''
  if (file.type.startsWith('image/') || ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp'].includes(extension)) {
    return 'Image'
  }
  if (file.type === 'application/pdf' || extension === 'pdf') {
    return 'PDF'
  }
  if (extension === 'docx') {
    return 'DOCX'
  }
  if (['md', 'markdown', 'txt', 'text'].includes(extension) || file.type.startsWith('text/')) {
    return 'Text'
  }
  return extension ? extension.toUpperCase() : 'File'
}

function createLocalUploadRow(file: File, libraryName: string): DocumentRow {
  return {
    id: `local-upload:${crypto.randomUUID()}`,
    logicalDocumentId: null,
    fileName: file.name,
    fileType: inferFileType(file),
    fileSizeLabel: formatFileSizeLabel(file.size),
    uploadedAt: new Date().toISOString(),
    libraryName,
    stage: 'client_uploading',
    status: 'queued',
    progressPercent: 0,
    activityStatus: 'active',
    lastActivityAt: new Date().toISOString(),
    stalledReason: null,
    chunkCount: null,
    graphNodeCount: null,
    graphEdgeCount: null,
    activeRevisionNo: null,
    activeRevisionKind: null,
    latestAttemptNo: 0,
    accountingStatus: 'unpriced',
    totalEstimatedCost: null,
    settledEstimatedCost: null,
    inFlightEstimatedCost: null,
    currency: null,
    inFlightStageCount: 0,
    missingStageCount: 0,
    partialHistory: false,
    partialHistoryReason: null,
    mutation: {
      kind: null,
      status: null,
      warning: null,
    },
    canRetry: false,
    canAppend: false,
    canReplace: false,
    canRemove: false,
    detailAvailable: false,
  }
}

async function processWithConcurrency<T>(
  items: T[],
  limit: number,
  worker: (item: T) => Promise<void>,
): Promise<void> {
  let nextIndex = 0
  const workers = Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (nextIndex < items.length) {
      const currentIndex = nextIndex
      nextIndex += 1
      const currentItem = items[currentIndex]
      if (currentItem === undefined) {
        return
      }
      await worker(currentItem)
    }
  })
  await Promise.all(workers)
}

export const useDocumentsStore = defineStore('documents', {
  state: (): DocumentsState => ({
    surface: null,
    localUploadRows: [],
    detail: null,
    loading: false,
    uploadLoading: false,
    detailLoading: false,
    mutationLoading: false,
    error: null,
    detailError: null,
    searchQuery: '',
    statusFilter: '',
    accountingFilter: '',
    mutationStatusFilter: '',
    fileTypeFilter: '',
    detailOpen: false,
    appendDialogDocumentId: null,
    replaceDialogDocumentId: null,
    uploadFailures: [],
  }),
  getters: {
    filteredRows(state): DocumentRow[] {
      if (!state.surface) {
        return []
      }

      return state.surface.rows.filter((row) => {
        const matchesSearch =
          state.searchQuery.trim().length === 0 ||
          row.fileName.toLowerCase().includes(state.searchQuery.trim().toLowerCase())
        const matchesStatus =
          state.statusFilter === '' || row.status === state.statusFilter
        const matchesAccounting =
          state.accountingFilter === '' || row.accountingStatus === state.accountingFilter
        const matchesMutation =
          state.mutationStatusFilter === '' || row.mutation.status === state.mutationStatusFilter
        const matchesType =
          state.fileTypeFilter === '' || row.fileType === state.fileTypeFilter

        return matchesSearch && matchesStatus && matchesAccounting && matchesMutation && matchesType
      })
    },
    appendDialogDocument(state): DocumentRow | null {
      return state.surface?.rows.find((row) => row.id === state.appendDialogDocumentId) ?? null
    },
    replaceDialogDocument(state): DocumentRow | null {
      return state.surface?.rows.find((row) => row.id === state.replaceDialogDocumentId) ?? null
    },
    refreshIntervalMs(state): number {
      return resolveRefreshInterval(
        state.surface?.rows ?? [],
        state.detail,
        state.detailOpen,
        state.surface?.diagnostics.activeBacklogCount ?? 0,
      )
    },
  },
  actions: {
    syncSurfaceWithLocalUploads(): void {
      if (this.surface) {
        this.surface = mergeLocalUploadRows(this.surface, this.localUploadRows)
      }
    },
    upsertAcceptedRow(row: DocumentRow, placeholderId: string): void {
      this.localUploadRows = this.localUploadRows.filter((item) => item.id !== placeholderId)
      const currentSurface = this.surface ?? createEmptySurface()
      const rows = currentSurface.rows.some((item) => item.id === placeholderId)
        ? replaceRow(currentSurface.rows, placeholderId, row)
        : [row, ...currentSurface.rows.filter((item) => item.id !== row.id)]
      this.surface = withRows(currentSurface, rows)
    },
    removeLocalUploadRow(placeholderId: string): void {
      this.localUploadRows = this.localUploadRows.filter((item) => item.id !== placeholderId)
      if (!this.surface) {
        return
      }
      this.surface = withRows(this.surface, removeRow(this.surface.rows, placeholderId))
    },
    clearUploadFailures(): void {
      this.uploadFailures = []
    },
    async loadSurface(options?: { syncDetail?: boolean }): Promise<void> {
      this.loading = true
      this.error = null
      try {
        this.surface = mergeLocalUploadRows(await fetchDocumentsSurface(), this.localUploadRows)
        this.detail = syncDetailContributionSummaryFromRow(
          this.detail,
          this.surface.rows.find((row) => row.id === this.detail?.id),
        )
        if (options?.syncDetail) {
          await this.syncOpenDetail().catch(() => undefined)
        }
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to load documents surface'
        throw error
      } finally {
        this.loading = false
      }
    },
    async loadDetail(id: string, options?: { silent?: boolean }): Promise<DocumentDetail> {
      if (!options?.silent) {
        this.detailLoading = true
      }
      this.detailError = null
      try {
        const detail = await fetchDocumentDetail(id)
        if (this.detailOpen) {
          this.detail = detail
        } else if (!options?.silent) {
          this.detail = detail
        }
        return detail
      } catch (error) {
        this.detailError =
          error instanceof Error ? error.message : 'Failed to load document detail'
        if (!options?.silent) {
          this.detail = null
        }
        throw error
      } finally {
        if (!options?.silent) {
          this.detailLoading = false
        }
      }
    },
    async syncOpenDetail(): Promise<void> {
      if (!this.detailOpen || !this.detail?.id) {
        return
      }
      const selectedId = this.detail.id
      const detail = await fetchDocumentDetail(selectedId)
      if (this.detail.id === selectedId) {
        this.detail = detail
        this.detailError = null
      }
    },
    async uploadFiles(files: File[]): Promise<void> {
      if (files.length === 0) {
        return
      }

      this.uploadLoading = true
      this.error = null
      this.clearUploadFailures()
      const graphStore = useGraphStore() as {
        loadSurface: (libraryId: string, options?: { preserveUi?: boolean }) => Promise<void>
      }
      const shellStore = useShellStore()
      const libraryId = shellStore.context?.activeLibrary.id
      const libraryName = shellStore.context?.activeLibrary.name ?? ''
      const placeholders = files.map((file) => createLocalUploadRow(file, libraryName))
      const queuedFiles: { file: File; placeholderId: string }[] = placeholders.map(
        (placeholder, index) => ({
          file: files[index],
          placeholderId: placeholder.id,
        }),
      )
      this.localUploadRows = [...placeholders, ...this.localUploadRows]
      this.surface = mergeLocalUploadRows(this.surface ?? createEmptySurface(), this.localUploadRows)
      const failures: DocumentUploadFailure[] = []
      try {
        await processWithConcurrency(
          queuedFiles,
          LOCAL_UPLOAD_CONCURRENCY,
          async ({ file, placeholderId }) => {
            try {
              const row = await uploadDocument(file)
              this.upsertAcceptedRow(row, placeholderId)
            } catch (error) {
              this.removeLocalUploadRow(placeholderId)
              failures.push(normalizeDocumentUploadFailure(file, error))
            }
          },
        )
        this.uploadFailures = failures
        await this.loadSurface({ syncDetail: true })
        if (libraryId) {
          await graphStore.loadSurface(libraryId, { preserveUi: true }).catch(() => undefined)
        }
        if (failures.length > 0) {
          const firstFailure = failures[0]
          this.error =
            failures.length === 1
              ? firstFailure.message
              : `${String(failures.length)} files failed to upload. First error: ${firstFailure.message}`
        }
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to upload documents'
        throw error
      } finally {
        this.uploadLoading = false
      }
    },
    async openDetail(id: string): Promise<void> {
      this.detailOpen = true
      try {
        this.detail = await this.loadDetail(id)
      } catch (error) {
        this.detail = null
        throw error
      }
    },
    closeDetail(): void {
      this.detailOpen = false
    },
    openAppendDialog(id: string): void {
      this.replaceDialogDocumentId = null
      this.appendDialogDocumentId = id
    },
    closeAppendDialog(): void {
      this.appendDialogDocumentId = null
    },
    openReplaceDialog(id: string): void {
      this.appendDialogDocumentId = null
      this.replaceDialogDocumentId = id
    },
    closeReplaceDialog(): void {
      this.replaceDialogDocumentId = null
    },
    async retryDocument(id: string): Promise<void> {
      const graphStore = useGraphStore() as {
        loadSurface: (libraryId: string, options?: { preserveUi?: boolean }) => Promise<void>
      }
      const libraryId = useShellStore().context?.activeLibrary.id
      await retryDocumentItem(id)
      await this.loadSurface({ syncDetail: true })
      if (libraryId) {
        await graphStore.loadSurface(libraryId, { preserveUi: true }).catch(() => undefined)
      }
      if (this.detail?.id === id) {
        await this.openDetail(id)
      }
    },
    async removeDocument(id: string): Promise<void> {
      const graphStore = useGraphStore() as {
        loadSurface: (libraryId: string, options?: { preserveUi?: boolean }) => Promise<void>
      }
      const libraryId = useShellStore().context?.activeLibrary.id
      await deleteDocumentItem(id)
      await this.loadSurface({ syncDetail: true })
      if (libraryId) {
        await graphStore.loadSurface(libraryId, { preserveUi: true }).catch(() => undefined)
      }
      if (this.detail?.id === id) {
        try {
          await this.loadDetail(id)
        } catch {
          this.detail = null
          this.detailOpen = false
        }
      }
    },
    async reprocessDocument(id: string): Promise<void> {
      const graphStore = useGraphStore() as {
        loadSurface: (libraryId: string, options?: { preserveUi?: boolean }) => Promise<void>
      }
      const libraryId = useShellStore().context?.activeLibrary.id
      await reprocessDocumentItem(id)
      await this.loadSurface({ syncDetail: true })
      if (libraryId) {
        await graphStore.loadSurface(libraryId, { preserveUi: true }).catch(() => undefined)
      }
      if (this.detail?.id === id) {
        await this.openDetail(id)
      }
    },
    async submitAppendDocument(id: string, content: string): Promise<DocumentMutationAccepted> {
      const shellStore = useShellStore()
      const graphStore = useGraphStore() as {
        loadSurface: (libraryId: string, options?: { preserveUi?: boolean }) => Promise<void>
      }
      const libraryId = shellStore.context?.activeLibrary.id
      if (!libraryId) {
        throw new Error('Active library is not selected')
      }

      this.mutationLoading = true
      this.error = null
      try {
        const mutation = await appendDocumentItem(libraryId, id, content)
        this.appendDialogDocumentId = null
        await this.loadSurface({ syncDetail: true })
        await graphStore.loadSurface(libraryId, { preserveUi: true }).catch(() => undefined)
        if (this.detail?.id === id && this.detailOpen) {
          await this.loadDetail(id)
        }
        return mutation
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to append document content'
        throw error
      } finally {
        this.mutationLoading = false
      }
    },
    async submitReplaceDocument(id: string, file: File): Promise<DocumentMutationAccepted> {
      const shellStore = useShellStore()
      const graphStore = useGraphStore() as {
        loadSurface: (libraryId: string, options?: { preserveUi?: boolean }) => Promise<void>
      }
      const libraryId = shellStore.context?.activeLibrary.id
      if (!libraryId) {
        throw new Error('Active library is not selected')
      }

      this.mutationLoading = true
      this.error = null
      try {
        const mutation = await replaceDocumentItem(libraryId, id, file)
        this.replaceDialogDocumentId = null
        await this.loadSurface({ syncDetail: true })
        await graphStore.loadSurface(libraryId, { preserveUi: true }).catch(() => undefined)
        if (this.detail?.id === id && this.detailOpen) {
          await this.loadDetail(id)
        }
        return mutation
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to replace document file'
        throw error
      } finally {
        this.mutationLoading = false
      }
    },
    setSearchQuery(value: string): void {
      this.searchQuery = value
    },
    setStatusFilter(value: DocumentStatus | ''): void {
      this.statusFilter = value
    },
    setAccountingFilter(value: DocumentAccountingStatus | ''): void {
      this.accountingFilter = value
    },
    setMutationStatusFilter(value: DocumentMutationStatus | ''): void {
      this.mutationStatusFilter = value
    },
    setFileTypeFilter(value: string): void {
      this.fileTypeFilter = value
    },
  },
})
