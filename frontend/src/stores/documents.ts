import { defineStore } from 'pinia'
import type {
  DocumentDetail,
  DocumentDisplayStatus,
  DocumentMutationAccepted,
  DocumentRowSummary,
  DocumentStatus,
  DocumentUploadFailure,
  DocumentsWorkspaceSurface,
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
import { i18n } from 'src/lib/i18n'
import { useGraphStore } from './graph'
import { useShellStore } from './shell'

interface DocumentsState {
  workspace: DocumentsWorkspaceSurface
  mutationLoading: boolean
  mutationError: string | null
  appendDialogDocumentId: string | null
  replaceDialogDocumentId: string | null
}

const LOCAL_UPLOAD_CONCURRENCY = 3
const REFRESH_INTERVAL_MS = 4_000

function formatDateTime(value: string | null): string {
  if (!value) {
    return '—'
  }
  const parsed = new Date(value)
  if (Number.isNaN(parsed.getTime())) {
    return value
  }
  return new Intl.DateTimeFormat(i18n.global.locale.value || undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(parsed)
}

function statusLabelFor(status: DocumentStatus): string {
  const key = `documents.statuses.${status}`
  return i18n.global.te(key) ? i18n.global.t(key) : status
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

function createEmptyWorkspace(): DocumentsWorkspaceSurface {
  return {
    acceptedFormats: [],
    maxSizeMb: 50,
    loading: false,
    error: null,
    counters: {
      queued: 0,
      processing: 0,
      ready: 0,
      readyNoGraph: 0,
      failed: 0,
    },
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
    uploadInProgress: false,
    uploadFailures: [],
    uploadQueue: [],
    selectedDocumentId: null,
  }
}

function createUploadPlaceholder(file: File): DocumentRowSummary {
  const createdAt = new Date().toISOString()
  return {
    id: `local-upload:${crypto.randomUUID()}`,
    fileName: file.name,
    fileType: inferFileType(file),
    fileSizeLabel: formatFileSizeLabel(file.size),
    uploadedAt: createdAt,
    status: 'queued',
    statusLabel: statusLabelFor('queued'),
    activityLabel: formatDateTime(createdAt),
    mutationLabel: null,
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
    workspace: createEmptyWorkspace(),
    mutationLoading: false,
    mutationError: null,
    appendDialogDocumentId: null,
    replaceDialogDocumentId: null,
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
          if (statusFilter === 'in_progress') {
            return row.status === 'queued' || row.status === 'processing'
          }
          if (statusFilter === 'ready') {
            return row.status === 'ready' || row.status === 'ready_no_graph'
          }
          return row.status === 'failed'
        })

      const direction = state.workspace.filters.sortDirection === 'asc' ? 1 : -1
      const sorted = rows.slice().sort((left, right) => {
        if (state.workspace.filters.sortField === 'fileName') {
          return left.fileName.localeCompare(right.fileName) * direction
        }
        return right.uploadedAt.localeCompare(left.uploadedAt) * direction
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
    refreshIntervalMs(state): number {
      const activeCount = state.workspace.counters.queued + state.workspace.counters.processing
      const inspectorStatus = state.workspace.inspector.detail?.status ?? null
      const inspectorActive =
        inspectorStatus === 'queued' || inspectorStatus === 'processing'
      return activeCount > 0 || inspectorActive ? REFRESH_INTERVAL_MS : 0
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

        const surface = await fetchDocumentsSurface()
        this.workspace.acceptedFormats = surface.acceptedFormats
        this.workspace.maxSizeMb = surface.maxSizeMb
        this.workspace.counters = surface.counters
        this.workspace.rows = surface.rows
        if (options?.syncInspector) {
          await this.refreshInspector().catch(() => undefined)
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
    async loadDetail(id: string, options?: { silent?: boolean }): Promise<DocumentDetail> {
      if (!options?.silent) {
        this.workspace.inspector.loading = true
      }
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
    openAppendDialog(id: string): void {
      this.replaceDialogDocumentId = null
      this.appendDialogDocumentId = id
    },
    closeAppendDialog(): void {
      this.appendDialogDocumentId = null
      this.mutationError = null
    },
    openReplaceDialog(id: string): void {
      this.appendDialogDocumentId = null
      this.replaceDialogDocumentId = id
    },
    closeReplaceDialog(): void {
      this.replaceDialogDocumentId = null
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
        this.workspace.error = 'Active workspace and library are required before uploading documents'
        return
      }

      const placeholders = files.map((file) => createUploadPlaceholder(file))
      const queuedFiles: { file: File; placeholderId: string }[] = placeholders.map(
        (placeholder, index) => ({
          file: files[index],
          placeholderId: placeholder.id,
        }),
      )
      this.workspace.uploadQueue = [...placeholders, ...this.workspace.uploadQueue]
      const failures: DocumentUploadFailure[] = []
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
              this.workspace.rows = [row, ...this.workspace.rows.filter((item) => item.id !== row.id)]
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
        this.workspace.error =
          error instanceof Error ? error.message : 'Failed to upload documents'
        throw error
      } finally {
        this.workspace.uploadInProgress = false
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
      await deleteDocumentItem(id)
      await this.loadWorkspace({ syncInspector: true })
      if (libraryId) {
        await graphStore.loadSurface(libraryId, { preserveUi: true }).catch(() => undefined)
      }
      if (this.workspace.selectedDocumentId === id) {
        this.closeDetail()
      }
    },
    async reprocessDocument(id: string): Promise<void> {
      const graphStore = useGraphStore() as {
        loadSurface: (libraryId: string, options?: { preserveUi?: boolean }) => Promise<void>
      }
      const libraryId = useShellStore().activeLibrary?.id ?? null
      await reprocessDocumentItem(id)
      await this.loadWorkspace({ syncInspector: true })
      if (libraryId) {
        await graphStore.loadSurface(libraryId, { preserveUi: true }).catch(() => undefined)
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
