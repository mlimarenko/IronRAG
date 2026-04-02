import { defineStore } from 'pinia'
import type {
  CreateWebIngestRunInput,
  DocumentDetail,
  DocumentDisplayStatus,
  DocumentMutationAccepted,
  DocumentRowSummary,
  WebIngestRun,
  DocumentsSortField,
  DocumentStatus,
  DocumentUploadFailure,
  DocumentsWorkspaceSurface,
  WebIngestRunReceipt,
} from 'src/models/ui/documents'
import { inferDocumentFileType, isAcceptedDocumentUpload } from 'src/models/ui/documentFormats'
import {
  appendDocumentItem,
  cancelWebIngestRun,
  createWebIngestRun,
  deleteDocumentItem,
  fetchDocumentDetail,
  fetchDocumentsSurface,
  fetchWebIngestRun,
  fetchWebIngestRunPages,
  fetchWebIngestRuns,
  fetchLibraryCostSummary,
  normalizeDocumentUploadFailure,
  replaceDocumentItem,
  retryDocumentItem,
  uploadDocument,
} from 'src/services/api/documents'
import { i18n } from 'src/lib/i18n'
import { useGraphStore } from './graph'
import { useShellStore } from './shell'

interface DocumentsState {
  workspace: DocumentsWorkspaceSurface
  mutationLoading: boolean
  mutationError: string | null
  addLinkDialogOpen: boolean
  webRunLoading: boolean
  webRunError: string | null
  lastAcceptedWebRun: WebIngestRunReceipt | null
  appendDialogDocumentId: string | null
  replaceDialogDocumentId: string | null
  deleteDialogDocumentId: string | null
}

const LOCAL_UPLOAD_CONCURRENCY = 3
const REFRESH_INTERVAL_MS = 4_000

function defaultSortDirectionFor(field: DocumentsSortField): 'asc' | 'desc' {
  switch (field) {
    case 'uploadedAt':
    case 'fileSizeBytes':
    case 'costAmount':
    case 'status':
      return 'desc'
    case 'fileName':
    case 'fileType':
    default:
      return 'asc'
  }
}

function compareUploadedAt(left: string, right: string): number {
  return Date.parse(left) - Date.parse(right)
}

function compareNullableNumber(left: number | null, right: number | null): number {
  if (left === null && right === null) {
    return 0
  }
  if (left === null) {
    return 1
  }
  if (right === null) {
    return -1
  }
  return left - right
}

function compareNullableString(left: string | null, right: string | null): number {
  if (left === null && right === null) {
    return 0
  }
  if (left === null) {
    return 1
  }
  if (right === null) {
    return -1
  }
  return left.localeCompare(right)
}

function rowReadinessRank(row: DocumentRowSummary): number {
  switch (row.preparation?.readinessKind ?? row.status) {
    case 'failed':
      return 5
    case 'processing':
    case 'queued':
      return 4
    case 'readable':
      return 3
    case 'graph_sparse':
      return 2
    case 'graph_ready':
    case 'ready':
    default:
      return 1
  }
}

function compareRowsBySortField(
  left: DocumentRowSummary,
  right: DocumentRowSummary,
  field: DocumentsSortField,
  direction: 'asc' | 'desc',
): number {
  const applyDirection = (value: number): number => (direction === 'asc' ? value : -value)

  switch (field) {
    case 'fileName':
      return applyDirection(left.fileName.localeCompare(right.fileName))
    case 'fileType':
      return applyDirection(compareNullableString(left.fileType, right.fileType))
    case 'fileSizeBytes':
      return applyDirection(compareNullableNumber(left.fileSizeBytes, right.fileSizeBytes))
    case 'costAmount':
      return applyDirection(compareNullableNumber(left.costAmount, right.costAmount))
    case 'status':
      return applyDirection(rowReadinessRank(left) - rowReadinessRank(right))
    case 'uploadedAt':
    default:
      return applyDirection(compareUploadedAt(left.uploadedAt, right.uploadedAt))
  }
}

function statusLabelFor(status: DocumentStatus): string {
  const key = `documents.statuses.${status}`
  return i18n.global.te(key) ? i18n.global.t(key) : status
}

function documentStageLabel(stage: string): string {
  const key = `documents.stage.${stage}`
  return i18n.global.te(key) ? i18n.global.t(key) : stage
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
  return inferDocumentFileType(file.name, file.type || null)
}

function createUnsupportedUploadFailure(file: File): DocumentUploadFailure {
  return {
    fileName: file.name,
    message: i18n.global.t('documents.dialogs.replace.validationType'),
    errorKind: 'unsupported_upload_type',
    rejectionKind: 'unsupported_upload_type',
    detectedFormat: inferFileType(file),
    mimeType: file.type || null,
    fileSizeBytes: file.size,
    uploadLimitMb: null,
    rejectionCause: i18n.global.t('documents.uploadReport.rejectionKinds.unsupported_upload_type'),
    operatorAction: null,
  }
}

function createEmptyWorkspace(): DocumentsWorkspaceSurface {
  return {
    acceptedFormats: [],
    maxSizeMb: 50,
    loading: false,
    error: null,
    webRuns: [],
    counters: {
      processing: 0,
      readable: 0,
      graphSparse: 0,
      graphReady: 0,
      failed: 0,
    },
    costSummary: null,
    rows: [],
    filters: {
      searchQuery: '',
      statusFilter: '',
      selectedFileTypes: [],
      sortField: 'uploadedAt',
      sortDirection: 'desc',
    },
    inspector: {
      documentId: null,
      loading: false,
      error: null,
      detail: null,
    },
    webRunInspector: {
      runId: null,
      loading: false,
      error: null,
      detail: null,
      pages: [],
    },
    webRunActionRunId: null,
    uploadInProgress: false,
    uploadFailures: [],
    uploadQueue: [],
    selectedDocumentId: null,
    selectedWebRunId: null,
  }
}

function normalizeWorkspaceCounters(
  counters: Partial<DocumentsWorkspaceSurface['counters']> | null | undefined,
): DocumentsWorkspaceSurface['counters'] {
  const fallback = createEmptyWorkspace().counters
  return {
    processing: counters?.processing ?? fallback.processing,
    readable: counters?.readable ?? fallback.readable,
    graphSparse: counters?.graphSparse ?? fallback.graphSparse,
    graphReady: counters?.graphReady ?? fallback.graphReady,
    failed: counters?.failed ?? fallback.failed,
  }
}

function createUploadPlaceholder(file: File): DocumentRowSummary {
  const createdAt = new Date().toISOString()
  return {
    id: `local-upload:${crypto.randomUUID()}`,
    fileName: file.name,
    fileType: inferFileType(file),
    fileSizeBytes: file.size,
    fileSizeLabel: formatFileSizeLabel(file.size),
    uploadedAt: createdAt,
    status: 'queued',
    statusLabel: statusLabelFor('queued'),
    stage: 'client_uploading',
    stageLabel: documentStageLabel('client_uploading'),
    progressPercent: 4,
    activityStatus: 'active',
    lastActivityAt: createdAt,
    stalledReason: null,
    costAmount: null,
    costLabel: null,
    failureMessage: null,
    canRetry: false,
    detailAvailable: false,
    preparation: null,
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
    workspace: createEmptyWorkspace(),
    mutationLoading: false,
    mutationError: null,
    addLinkDialogOpen: false,
    webRunLoading: false,
    webRunError: null,
    lastAcceptedWebRun: null,
    appendDialogDocumentId: null,
    replaceDialogDocumentId: null,
    deleteDialogDocumentId: null,
  }),
  getters: {
    mergedRows(state): DocumentRowSummary[] {
      const merged: DocumentRowSummary[] = []
      const seen = new Set<string>()
      for (const row of [...state.workspace.uploadQueue, ...state.workspace.rows]) {
        if (seen.has(row.id)) {
          continue
        }
        merged.push(row)
        seen.add(row.id)
      }
      return merged
    },
    filteredRows(state): DocumentRowSummary[] {
      const query = state.workspace.filters.searchQuery.trim().toLowerCase()
      const statusFilter = state.workspace.filters.statusFilter
      const rows = this.mergedRows
        .filter((row) => {
          if (!query) {
            return true
          }
          return row.fileName.toLowerCase().includes(query)
        })
        .filter((row) => {
          if (!statusFilter) {
            return true
          }
          const readiness = row.preparation?.readinessKind ?? row.status
          if (statusFilter === 'in_progress') {
            return readiness === 'processing' || readiness === 'queued'
          }
          if (statusFilter === 'ready') {
            return (
              readiness === 'readable' ||
              readiness === 'graph_sparse' ||
              readiness === 'graph_ready' ||
              readiness === 'ready' ||
              readiness === 'ready_no_graph'
            )
          }
          return readiness === 'failed'
        })

      const sorted = rows.slice().sort((left, right) => {
        const primary = compareRowsBySortField(
          left,
          right,
          state.workspace.filters.sortField,
          state.workspace.filters.sortDirection,
        )
        if (primary !== 0) {
          return primary
        }

        const uploadedAtTieBreak = compareUploadedAt(right.uploadedAt, left.uploadedAt)
        if (uploadedAtTieBreak !== 0) {
          return uploadedAtTieBreak
        }

        return left.fileName.localeCompare(right.fileName)
      })
      return sorted
    },
    appendDialogDocument(state): DocumentRowSummary | null {
      if (!state.appendDialogDocumentId) {
        return null
      }
      return state.workspace.rows.find((row) => row.id === state.appendDialogDocumentId) ?? null
    },
    replaceDialogDocument(state): DocumentRowSummary | null {
      if (!state.replaceDialogDocumentId) {
        return null
      }
      return state.workspace.rows.find((row) => row.id === state.replaceDialogDocumentId) ?? null
    },
    deleteDialogDocument(state): DocumentRowSummary | null {
      if (!state.deleteDialogDocumentId) {
        return null
      }
      return state.workspace.rows.find((row) => row.id === state.deleteDialogDocumentId) ?? null
    },
    refreshIntervalMs(state): number {
      const activeCount = normalizeWorkspaceCounters(state.workspace.counters).processing
      const hasActiveWebRuns = state.workspace.webRuns.some((run) =>
        ['accepted', 'discovering', 'processing'].includes(run.runState),
      )
      const inspectorStatus = state.workspace.inspector.detail?.status ?? null
      const inspectorReadiness =
        state.workspace.inspector.detail?.preparation?.readinessKind ?? null
      const inspectorActive =
        inspectorStatus === 'queued' ||
        inspectorStatus === 'processing' ||
        inspectorReadiness === 'processing' ||
        inspectorReadiness === 'readable' ||
        inspectorReadiness === 'graph_sparse'
      return activeCount > 0 || inspectorActive || hasActiveWebRuns ? REFRESH_INTERVAL_MS : 0
    },
  },
  actions: {
    clearUploadFailures(): void {
      this.workspace.uploadFailures = []
    },
    setSearchQuery(value: string): void {
      this.workspace.filters.searchQuery = value
    },
    setStatusFilter(value: DocumentDisplayStatus | ''): void {
      this.workspace.filters.statusFilter = value
    },
    openAddLinkDialog(): void {
      this.webRunError = null
      this.addLinkDialogOpen = true
    },
    closeAddLinkDialog(): void {
      if (this.webRunLoading) {
        return
      }
      this.webRunError = null
      this.addLinkDialogOpen = false
    },
    toggleSort(field: DocumentsSortField): void {
      if (this.workspace.filters.sortField === field) {
        this.workspace.filters.sortDirection =
          this.workspace.filters.sortDirection === 'asc' ? 'desc' : 'asc'
        return
      }
      this.workspace.filters.sortField = field
      this.workspace.filters.sortDirection = defaultSortDirectionFor(field)
    },
    async loadWorkspace(options?: { syncInspector?: boolean }): Promise<void> {
      this.workspace.loading = true
      this.workspace.error = null
      try {
        const shellStore = useShellStore()
        const activeWorkspace = shellStore.activeWorkspace
        const activeLibrary = shellStore.activeLibrary

        if (!activeWorkspace || !activeLibrary) {
          this.workspace = createEmptyWorkspace()
          return
        }

        const [surface, costSummary, webRuns] = await Promise.all([
          fetchDocumentsSurface(),
          fetchLibraryCostSummary(activeLibrary.id),
          fetchWebIngestRuns(activeLibrary.id),
        ])
        this.workspace.acceptedFormats = surface.acceptedFormats
        this.workspace.maxSizeMb = surface.maxSizeMb
        this.workspace.counters = normalizeWorkspaceCounters(surface.counters)
        this.workspace.costSummary = costSummary
        this.workspace.rows = surface.rows
        this.workspace.webRuns = webRuns
        if (options?.syncInspector) {
          await this.refreshInspector().catch(() => undefined)
          await this.refreshWebRunInspector().catch(() => undefined)
        }
      } catch (error) {
        this.workspace.error =
          error instanceof Error ? error.message : 'Failed to load documents workspace'
        throw error
      } finally {
        this.workspace.loading = false
      }
    },
    async refreshInspector(): Promise<void> {
      const documentId = this.workspace.selectedDocumentId
      if (!documentId) {
        return
      }
      await this.loadDetail(documentId, { silent: true })
    },
    async refreshWebRunInspector(): Promise<void> {
      const runId = this.workspace.selectedWebRunId
      if (!runId) {
        return
      }
      await this.loadWebRun(runId, { silent: true })
    },
    async loadDetail(id: string, options?: { silent?: boolean }): Promise<DocumentDetail> {
      if (!options?.silent) {
        this.workspace.inspector.loading = true
      }
      this.workspace.inspector.documentId = id
      this.workspace.inspector.error = null
      try {
        const detail = await fetchDocumentDetail(id)
        this.workspace.inspector.detail = detail
        this.workspace.inspector.documentId = id
        return detail
      } catch (error) {
        this.workspace.inspector.error =
          error instanceof Error ? error.message : 'Failed to load document detail'
        if (!options?.silent) {
          this.workspace.inspector.detail = null
        }
        throw error
      } finally {
        if (!options?.silent) {
          this.workspace.inspector.loading = false
        }
      }
    },
    async openDetail(id: string): Promise<void> {
      const normalizedId = id.trim()
      if (!normalizedId || normalizedId.startsWith('local-upload:')) {
        this.closeDetail()
        return
      }
      const row = this.workspace.rows.find((item) => item.id === normalizedId) ?? null
      if (row && !row.detailAvailable) {
        this.closeDetail()
        return
      }
      this.closeWebRun()
      this.workspace.selectedDocumentId = normalizedId
      await this.loadDetail(normalizedId)
    },
    closeDetail(): void {
      this.workspace.selectedDocumentId = null
      this.workspace.inspector = {
        documentId: null,
        loading: false,
        error: null,
        detail: null,
      }
    },
    async loadWebRun(runId: string, options?: { silent?: boolean }): Promise<WebIngestRun> {
      if (!options?.silent) {
        this.workspace.webRunInspector.loading = true
      }
      this.workspace.webRunInspector.runId = runId
      this.workspace.webRunInspector.error = null
      try {
        const [detail, pages] = await Promise.all([
          fetchWebIngestRun(runId),
          fetchWebIngestRunPages(runId),
        ])
        this.workspace.webRunInspector.detail = detail
        this.workspace.webRunInspector.pages = pages
        this.workspace.webRunInspector.runId = runId
        return detail
      } catch (error) {
        this.workspace.webRunInspector.error =
          error instanceof Error ? error.message : 'Failed to load web ingest run'
        if (!options?.silent) {
          this.workspace.webRunInspector.detail = null
          this.workspace.webRunInspector.pages = []
        }
        throw error
      } finally {
        if (!options?.silent) {
          this.workspace.webRunInspector.loading = false
        }
      }
    },
    async openWebRun(runId: string): Promise<void> {
      const normalizedId = runId.trim()
      if (!normalizedId) {
        this.closeWebRun()
        return
      }
      this.closeDetail()
      this.workspace.selectedWebRunId = normalizedId
      await this.loadWebRun(normalizedId)
    },
    closeWebRun(): void {
      this.workspace.selectedWebRunId = null
      this.workspace.webRunInspector = {
        runId: null,
        loading: false,
        error: null,
        detail: null,
        pages: [],
      }
    },
    openAppendDialog(id: string): void {
      this.deleteDialogDocumentId = null
      this.replaceDialogDocumentId = null
      this.appendDialogDocumentId = id
      this.mutationError = null
    },
    closeAppendDialog(): void {
      this.appendDialogDocumentId = null
      this.mutationError = null
    },
    openReplaceDialog(id: string): void {
      this.deleteDialogDocumentId = null
      this.appendDialogDocumentId = null
      this.replaceDialogDocumentId = id
      this.mutationError = null
    },
    closeReplaceDialog(): void {
      this.replaceDialogDocumentId = null
      this.mutationError = null
    },
    openDeleteDialog(id: string): void {
      this.appendDialogDocumentId = null
      this.replaceDialogDocumentId = null
      this.deleteDialogDocumentId = id
      this.mutationError = null
    },
    closeDeleteDialog(): void {
      this.deleteDialogDocumentId = null
      this.mutationError = null
    },
    async uploadFiles(files: File[]): Promise<void> {
      if (files.length === 0) {
        return
      }
      this.workspace.uploadInProgress = true
      this.workspace.error = null
      this.clearUploadFailures()
      const graphStore = useGraphStore() as {
        loadSurface: (libraryId: string, options?: { preserveUi?: boolean }) => Promise<void>
      }
      const shellStore = useShellStore()
      const activeWorkspace = shellStore.activeWorkspace
      const activeLibrary = shellStore.activeLibrary
      const libraryId = activeLibrary?.id ?? null
      if (!activeWorkspace || !activeLibrary) {
        this.workspace.uploadInProgress = false
        this.workspace.error =
          'Active workspace and library are required before uploading documents'
        return
      }

      const acceptedFiles: File[] = []
      const failures: DocumentUploadFailure[] = []
      for (const file of files) {
        if (!isAcceptedDocumentUpload(file, this.workspace.acceptedFormats)) {
          failures.push(createUnsupportedUploadFailure(file))
          continue
        }
        acceptedFiles.push(file)
      }
      if (acceptedFiles.length === 0) {
        this.workspace.uploadFailures = failures
        this.workspace.error = failures[0]?.message ?? 'Failed to upload documents'
        this.workspace.uploadInProgress = false
        return
      }

      const placeholders = acceptedFiles.map((file) => createUploadPlaceholder(file))
      const queuedFiles: { file: File; placeholderId: string }[] = placeholders.map(
        (placeholder, index) => ({
          file: acceptedFiles[index],
          placeholderId: placeholder.id,
        }),
      )
      this.workspace.uploadQueue = [...placeholders, ...this.workspace.uploadQueue]
      try {
        await processWithConcurrency(
          queuedFiles,
          LOCAL_UPLOAD_CONCURRENCY,
          async ({ file, placeholderId }) => {
            try {
              const row = await uploadDocument(file)
              this.workspace.uploadQueue = this.workspace.uploadQueue.filter(
                (item) => item.id !== placeholderId,
              )
              this.workspace.rows = [
                row,
                ...this.workspace.rows.filter((item) => item.id !== row.id),
              ]
            } catch (error) {
              this.workspace.uploadQueue = this.workspace.uploadQueue.filter(
                (item) => item.id !== placeholderId,
              )
              failures.push(normalizeDocumentUploadFailure(file, error))
            }
          },
        )
        this.workspace.uploadFailures = failures
        await this.loadWorkspace({ syncInspector: true })
        if (libraryId) {
          await graphStore.loadSurface(libraryId, { preserveUi: true }).catch(() => undefined)
        }
        if (failures.length > 0) {
          const firstFailure = failures[0]
          this.workspace.error =
            failures.length === 1
              ? firstFailure.message
              : `${String(failures.length)} files failed to upload. First error: ${firstFailure.message}`
        }
      } catch (error) {
        this.workspace.error = error instanceof Error ? error.message : 'Failed to upload documents'
        throw error
      } finally {
        this.workspace.uploadInProgress = false
      }
    },
    async submitWebIngestRun(
      input: Omit<CreateWebIngestRunInput, 'libraryId'>,
    ): Promise<WebIngestRunReceipt> {
      const libraryId = useShellStore().activeLibrary?.id ?? null
      if (!libraryId) {
        throw new Error('Active library is not selected')
      }

      this.webRunLoading = true
      this.webRunError = null
      try {
        const receipt = await createWebIngestRun({ ...input, libraryId })
        this.lastAcceptedWebRun = receipt
        this.addLinkDialogOpen = false
        await this.loadWorkspace({ syncInspector: true })
        await this.openWebRun(receipt.runId)
        return receipt
      } catch (error) {
        this.webRunError =
          error instanceof Error ? error.message : 'Failed to submit web ingest run'
        throw error
      } finally {
        this.webRunLoading = false
      }
    },
    async retryDocument(id: string): Promise<void> {
      const graphStore = useGraphStore() as {
        loadSurface: (libraryId: string, options?: { preserveUi?: boolean }) => Promise<void>
      }
      const libraryId = useShellStore().activeLibrary?.id ?? null
      await retryDocumentItem(id)
      await this.loadWorkspace({ syncInspector: true })
      if (libraryId) {
        await graphStore.loadSurface(libraryId, { preserveUi: true }).catch(() => undefined)
      }
    },
    async removeDocument(id: string): Promise<void> {
      const graphStore = useGraphStore() as {
        loadSurface: (libraryId: string, options?: { preserveUi?: boolean }) => Promise<void>
      }
      const libraryId = useShellStore().activeLibrary?.id ?? null
      this.mutationLoading = true
      this.mutationError = null
      try {
        await deleteDocumentItem(id)
        this.deleteDialogDocumentId = null
        await this.loadWorkspace({ syncInspector: true })
        if (libraryId) {
          await graphStore.loadSurface(libraryId, { preserveUi: true }).catch(() => undefined)
        }
        if (this.workspace.selectedDocumentId === id) {
          this.closeDetail()
        }
      } catch (error) {
        this.mutationError = error instanceof Error ? error.message : 'Failed to delete document'
        throw error
      } finally {
        this.mutationLoading = false
      }
    },
    async cancelWebRun(runId: string): Promise<WebIngestRunReceipt> {
      this.webRunLoading = true
      this.webRunError = null
      this.workspace.webRunActionRunId = runId
      try {
        const receipt = await cancelWebIngestRun(runId)
        await this.loadWorkspace({ syncInspector: true })
        if (this.workspace.selectedWebRunId === runId) {
          await this.loadWebRun(runId, { silent: true }).catch(() => undefined)
        }
        return receipt
      } catch (error) {
        this.webRunError =
          error instanceof Error ? error.message : 'Failed to cancel web ingest run'
        throw error
      } finally {
        this.workspace.webRunActionRunId = null
        this.webRunLoading = false
      }
    },
    async submitAppendDocument(id: string, content: string): Promise<DocumentMutationAccepted> {
      const shellStore = useShellStore()
      const graphStore = useGraphStore() as {
        loadSurface: (libraryId: string, options?: { preserveUi?: boolean }) => Promise<void>
      }
      const libraryId = shellStore.activeLibrary?.id ?? null
      if (!libraryId) {
        throw new Error('Active library is not selected')
      }

      this.mutationLoading = true
      this.mutationError = null
      try {
        const mutation = await appendDocumentItem(libraryId, id, content)
        this.appendDialogDocumentId = null
        await this.loadWorkspace({ syncInspector: true })
        await graphStore.loadSurface(libraryId, { preserveUi: true }).catch(() => undefined)
        if (this.workspace.selectedDocumentId === id) {
          await this.loadDetail(id, { silent: true })
        }
        return mutation
      } catch (error) {
        this.mutationError =
          error instanceof Error ? error.message : 'Failed to append document content'
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
      const libraryId = shellStore.activeLibrary?.id ?? null
      if (!libraryId) {
        throw new Error('Active library is not selected')
      }

      this.mutationLoading = true
      this.mutationError = null
      try {
        const mutation = await replaceDocumentItem(libraryId, id, file)
        this.replaceDialogDocumentId = null
        await this.loadWorkspace({ syncInspector: true })
        await graphStore.loadSurface(libraryId, { preserveUi: true }).catch(() => undefined)
        if (this.workspace.selectedDocumentId === id) {
          await this.loadDetail(id, { silent: true })
        }
        return mutation
      } catch (error) {
        this.mutationError =
          error instanceof Error ? error.message : 'Failed to replace document file'
        throw error
      } finally {
        this.mutationLoading = false
      }
    },
  },
})
