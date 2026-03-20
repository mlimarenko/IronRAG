import { defineStore } from 'pinia'
import type {
  DocumentAccountingStatus,
  DocumentActivityStatus,
  DocumentGraphHealthSummary,
  DocumentCollectionSettlementSummary,
  DocumentCollectionWarning,
  DocumentDetail,
  DocumentFilterValues,
  DocumentMutationAccepted,
  DocumentMutationStatus,
  DocumentProviderFailureSummary,
  DocumentQueueWaitingReason,
  DocumentRow,
  DocumentStatus,
  DocumentTerminalOutcomeSummary,
  DocumentUploadFailure,
  DocumentsWorkspaceNotice,
  DocumentsWorkspaceSummary,
  DocumentsSurfaceResponse,
} from 'src/models/ui/documents'
import type { GraphDiagnostics } from 'src/models/ui/graph'
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
import { fetchGraphDiagnostics } from 'src/services/api/graph'
import { i18n } from 'src/lib/i18n'
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
  informationalWarnings: DocumentCollectionWarning[]
  degradedWarnings: DocumentCollectionWarning[]
  settlementSnapshot: DocumentCollectionSettlementSummary | null
  terminalOutcomeSnapshot: DocumentTerminalOutcomeSummary | null
  graphHealthSnapshot: DocumentGraphHealthSummary | null
  graphDiagnostics: GraphDiagnostics | null
  providerFailureDetail: DocumentProviderFailureSummary | null
  workspaceSummary: DocumentsWorkspaceSummary | null
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
  const recoveryStatus = detail.extractedStats.recovery?.status ?? null
  const hasLiveRecoveryState =
    detail.status === 'processing' &&
    (recoveryStatus === 'recovered' || recoveryStatus === 'partial')
  const reconciliationScopeStatus = detail.reconciliationScope?.scopeStatus ?? null
  const hasLiveReconciliationState =
    reconciliationScopeStatus === 'pending' ||
    reconciliationScopeStatus === 'targeted' ||
    reconciliationScopeStatus === 'fallback_broad'
  return (
    needsActivityPolling(detail.activityStatus) ||
    hasPendingMutation(detail.mutation.status) ||
    hasLiveRecoveryState ||
    hasLiveReconciliationState
  )
}

function resolveRefreshInterval(
  rows: DocumentRow[],
  detail: DocumentDetail | null,
  detailOpen: boolean,
  activeBacklogCount: number,
  queueWaitingReason: DocumentQueueWaitingReason | null,
  terminalOutcome: DocumentTerminalOutcomeSummary | null,
  collectionGraphPollIntervalMs: number | null,
): number {
  if (terminalOutcome && terminalOutcome.terminalState !== 'live_in_flight') {
    return 0
  }
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
  if (queueWaitingReason === 'isolated_capacity_wait') {
    return WATCH_REFRESH_INTERVAL_MS
  }
  const backlogForCadence = effectiveBacklogCount + (pollDetail ? 1 : 0)
  const graphPollIntervalMs =
    detailOpen && detail?.graphThroughput
      ? detail.graphThroughput.recommendedPollIntervalMs
      : collectionGraphPollIntervalMs
  if (graphPollIntervalMs !== null && graphPollIntervalMs > 0) {
    if (backlogForCadence >= THROTTLED_BACKLOG_THRESHOLD) {
      return Math.max(graphPollIntervalMs, BACKLOG_REFRESH_INTERVAL_MS)
    }
    if (backlogForCadence >= WATCH_BACKLOG_THRESHOLD) {
      return Math.max(graphPollIntervalMs, WATCH_REFRESH_INTERVAL_MS)
    }
    return Math.max(graphPollIntervalMs, FAST_REFRESH_INTERVAL_MS)
  }
  if (backlogForCadence >= THROTTLED_BACKLOG_THRESHOLD) {
    return BACKLOG_REFRESH_INTERVAL_MS
  }
  if (backlogForCadence >= WATCH_BACKLOG_THRESHOLD) {
    return WATCH_REFRESH_INTERVAL_MS
  }
  return FAST_REFRESH_INTERVAL_MS
}

function resolveGraphDiagnosticsRefreshInterval(
  surface: DocumentsSurfaceResponse | null,
  graphHealth: DocumentGraphHealthSummary | null,
  graphDiagnostics: GraphDiagnostics | null,
): number {
  if (!surface) {
    return 0
  }
  const graphStatus = graphDiagnostics?.graphStatus ?? surface.graphStatus
  const projectionHealth = graphHealth?.projectionHealth ?? null
  const freshness = graphDiagnostics?.projectionFreshness ?? null
  const activeBacklogCount = surface.diagnostics.activeBacklogCount

  if (
    graphStatus === 'empty' &&
    !graphHealth &&
    activeBacklogCount === 0 &&
    surface.rebuildBacklogCount === 0
  ) {
    return 0
  }

  if (
    projectionHealth === 'failed' ||
    projectionHealth === 'retrying_contention' ||
    freshness === 'failed'
  ) {
    return FAST_REFRESH_INTERVAL_MS
  }

  if (
    freshness === 'stale' ||
    freshness === 'lagging' ||
    graphStatus === 'building' ||
    graphStatus === 'stale' ||
    activeBacklogCount > 0 ||
    surface.rebuildBacklogCount > 0
  ) {
    return WATCH_REFRESH_INTERVAL_MS
  }

  return 0
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
      queueIsolation: null,
      graphThroughput: null,
      settlement: null,
      terminalOutcome: null,
      graphHealth: null,
      warnings: [],
      perStage: [],
      perFormat: [],
    },
    workspace: null,
    rows: [],
  }
}

function splitCollectionWarnings(
  warnings: DocumentCollectionWarning[],
): {
  informationalWarnings: DocumentCollectionWarning[]
  degradedWarnings: DocumentCollectionWarning[]
} {
  return warnings.reduce(
    (accumulator, warning) => {
      if (warning.isDegraded) {
        accumulator.degradedWarnings.push(warning)
      } else {
        accumulator.informationalWarnings.push(warning)
      }
      return accumulator
    },
    {
      informationalWarnings: [] as DocumentCollectionWarning[],
      degradedWarnings: [] as DocumentCollectionWarning[],
    },
  )
}

function dedupeWorkspaceNotices(notices: DocumentsWorkspaceNotice[]): DocumentsWorkspaceNotice[] {
  const seen = new Set<string>()
  return notices.filter((notice) => {
    const key = `${notice.kind}:${notice.title}:${notice.message}`
    if (seen.has(key)) {
      return false
    }
    seen.add(key)
    return true
  })
}

function residualReasonLabel(reason: string): string {
  const key = `documents.terminal.residualReasons.${reason}`
  const translated = i18n.global.t(key)
  return translated === key ? reason : translated
}

function providerFailureClassLabel(value: string): string {
  const key = `documents.providerFailureClass.${value}`
  const translated = i18n.global.t(key)
  return translated === key ? value : translated
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
    graphThroughput: row.graphThroughput ?? detail.graphThroughput,
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
  const localId = `local-upload:${crypto.randomUUID()}`
  const createdAt = new Date().toISOString()

  return {
    id: localId,
    logicalDocumentId: null,
    readabilityState: 'unreadable',
    activeRevisionId: null,
    readableRevisionId: null,
    readableRevisionNo: null,
    fileName: file.name,
    fileType: inferFileType(file),
    fileSizeLabel: formatFileSizeLabel(file.size),
    uploadedAt: createdAt,
    libraryName,
    stage: 'client_uploading',
    status: 'queued',
    progressPercent: 0,
    activityStatus: 'active',
    lastActivityAt: createdAt,
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
    graphThroughput: null,
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
    canonical: {
      document: {
        id: localId,
        workspaceId: '',
        libraryId: '',
        externalKey: file.name,
        documentState: 'pending_upload',
        createdAt,
      },
      head: null,
      activeRevision: null,
      readableRevision: null,
      latestMutation: null,
      latestMutationItems: [],
      latestJob: null,
      latestAttempt: null,
      latestAttemptStages: [],
    },
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
    informationalWarnings: [],
    degradedWarnings: [],
    settlementSnapshot: null,
    terminalOutcomeSnapshot: null,
    graphHealthSnapshot: null,
    graphDiagnostics: null,
    providerFailureDetail: null,
    workspaceSummary: null,
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
    selectedDetailGraphQuality(state):
      | {
          graphNodeId: string | null
          canonicalSummaryPreview: DocumentDetail['canonicalSummaryPreview']
          reconciliationScope: DocumentDetail['reconciliationScope']
          extractionRecovery: DocumentDetail['extractedStats']['recovery']
          graphStats: DocumentDetail['graphStats']
          warningCount: number
          normalizationStatus: string
        }
      | null {
      if (!state.detail) {
        return null
      }
      return {
        graphNodeId: state.detail.graphNodeId,
        canonicalSummaryPreview: state.detail.canonicalSummaryPreview,
        reconciliationScope: state.detail.reconciliationScope,
        extractionRecovery: state.detail.extractedStats.recovery,
        graphStats: state.detail.graphStats,
        warningCount: state.detail.extractedStats.warningCount,
        normalizationStatus: state.detail.extractedStats.normalizationStatus,
      }
    },
    selectedDetailReconciliationScope(state): DocumentDetail['reconciliationScope'] {
      return state.detail?.reconciliationScope ?? null
    },
    selectedDetailCanonicalSummary(state): DocumentDetail['canonicalSummaryPreview'] {
      return state.detail?.canonicalSummaryPreview ?? null
    },
    workspacePrimarySummary(state): DocumentsWorkspaceSummary['primarySummary'] | null {
      return state.workspaceSummary?.primarySummary ?? null
    },
    workspaceSecondaryDiagnostics(state): DocumentsWorkspaceSummary['secondaryDiagnostics'] {
      return state.workspaceSummary?.secondaryDiagnostics ?? []
    },
    workspaceNoticeGroups(state): {
      degraded: DocumentsWorkspaceSummary['degradedNotices']
      informational: DocumentsWorkspaceSummary['informationalNotices']
    } {
      const degraded = [...(state.workspaceSummary?.degradedNotices ?? [])]
      const informational = [...(state.workspaceSummary?.informationalNotices ?? [])]
      const terminalOutcome = state.terminalOutcomeSnapshot
      const providerFailure = state.providerFailureDetail

      if (
        terminalOutcome?.terminalState === 'failed_with_residual_work' &&
        terminalOutcome.residualReason
      ) {
        degraded.push({
          kind: `residual:${terminalOutcome.residualReason}`,
          title: i18n.global.t('documents.workspace.notices.residualFailure.title', {
            reason: residualReasonLabel(terminalOutcome.residualReason),
          }),
          message: [
            terminalOutcome.failedDocumentCount > 0
              ? i18n.global.t('documents.workspace.notices.residualFailure.failedDocuments', {
                  count: terminalOutcome.failedDocumentCount,
                })
              : null,
            terminalOutcome.pendingGraphCount > 0
              ? i18n.global.t('documents.workspace.notices.residualFailure.pendingGraph', {
                  count: terminalOutcome.pendingGraphCount,
                })
              : null,
            terminalOutcome.queuedCount > 0
              ? i18n.global.t('documents.workspace.notices.residualFailure.queued', {
                  count: terminalOutcome.queuedCount,
                })
              : null,
            terminalOutcome.processingCount > 0
              ? i18n.global.t('documents.workspace.notices.residualFailure.processing', {
                  count: terminalOutcome.processingCount,
                })
              : null,
          ]
            .filter(Boolean)
            .join(' · '),
        })
      }

      if (terminalOutcome?.residualReason === 'provider_failure' && terminalOutcome.failedDocumentCount > 0) {
        degraded.push({
          kind: 'provider_failure_count',
          title: i18n.global.t('documents.workspace.notices.providerFailure.title'),
          message: i18n.global.t('documents.workspace.notices.providerFailure.message', {
            count: terminalOutcome.failedDocumentCount,
          }),
        })
      }

      if (providerFailure) {
        degraded.push({
          kind: `selected_provider_failure:${providerFailure.failureClass}`,
          title: i18n.global.t('documents.workspace.notices.selectedProviderFailure.title', {
            failure: providerFailureClassLabel(providerFailure.failureClass),
          }),
          message: [
            providerFailure.providerKind,
            providerFailure.modelName,
            providerFailure.requestShapeKey,
          ]
            .filter(Boolean)
            .join(' · '),
        })
      }

      return {
        degraded: dedupeWorkspaceNotices(degraded),
        informational: dedupeWorkspaceNotices(informational),
      }
    },
    selectedProviderFailureDetail(state): DocumentProviderFailureSummary | null {
      return state.providerFailureDetail ?? state.detail?.providerFailure ?? null
    },
    refreshIntervalMs(state): number {
      return resolveRefreshInterval(
        state.surface?.rows ?? [],
        state.detail,
        state.detailOpen,
        state.surface?.diagnostics.activeBacklogCount ?? 0,
        state.surface?.diagnostics.queueIsolation?.waitingReason ?? null,
        state.terminalOutcomeSnapshot,
        state.surface?.diagnostics.graphThroughput?.recommendedPollIntervalMs ?? null,
      )
    },
    graphDiagnosticsRefreshIntervalMs(state): number {
      return resolveGraphDiagnosticsRefreshInterval(
        state.surface,
        state.graphHealthSnapshot,
        state.graphDiagnostics,
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
        const shellStore = useShellStore()
        const activeWorkspace = shellStore.activeWorkspace
        const activeLibrary = shellStore.activeLibrary

        if (!activeWorkspace || !activeLibrary) {
          this.surface = createEmptySurface()
          this.informationalWarnings = []
          this.degradedWarnings = []
          this.settlementSnapshot = null
          this.terminalOutcomeSnapshot = null
          this.graphHealthSnapshot = null
          this.workspaceSummary = null
          this.providerFailureDetail = null
          this.graphDiagnostics = null
          return
        }

        this.surface = mergeLocalUploadRows(await fetchDocumentsSurface(), this.localUploadRows)
        const warningChannels = splitCollectionWarnings(this.surface.diagnostics.warnings)
        this.informationalWarnings = warningChannels.informationalWarnings
        this.degradedWarnings = warningChannels.degradedWarnings
        this.settlementSnapshot = this.surface.diagnostics.settlement
        this.terminalOutcomeSnapshot = this.surface.diagnostics.terminalOutcome
        this.graphHealthSnapshot = this.surface.diagnostics.graphHealth
        this.workspaceSummary = this.surface.workspace
        this.detail = syncDetailContributionSummaryFromRow(
          this.detail,
          this.surface.rows.find((row) => row.id === this.detail?.id),
        )
        this.providerFailureDetail = this.detail?.providerFailure ?? null
        if (this.graphDiagnosticsRefreshIntervalMs > 0 || this.graphDiagnostics === null) {
          await this.loadGraphDiagnostics({ silent: true }).catch(() => undefined)
        }
        if (options?.syncDetail) {
          await this.syncOpenDetail().catch(() => undefined)
        }
      } catch (error) {
        this.error = error instanceof Error ? error.message : 'Failed to load documents surface'
        this.settlementSnapshot = null
        this.terminalOutcomeSnapshot = null
        this.graphHealthSnapshot = null
        this.graphDiagnostics = null
        this.providerFailureDetail = null
        this.workspaceSummary = null
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
        this.providerFailureDetail = detail.providerFailure
        this.graphHealthSnapshot = detail.collectionDiagnostics?.graphHealth ?? this.graphHealthSnapshot
        return detail
      } catch (error) {
        this.detailError =
          error instanceof Error ? error.message : 'Failed to load document detail'
        if (!options?.silent) {
          this.detail = null
        }
        this.providerFailureDetail = null
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
        this.providerFailureDetail = detail.providerFailure
        this.graphHealthSnapshot = detail.collectionDiagnostics?.graphHealth ?? this.graphHealthSnapshot
        this.detailError = null
      }
    },
    async loadGraphDiagnostics(options?: { silent?: boolean }): Promise<void> {
      try {
        this.graphDiagnostics = await fetchGraphDiagnostics()
      } catch (error) {
        if (!options?.silent) {
          throw error
        }
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
      const activeWorkspace = shellStore.activeWorkspace
      const activeLibrary = shellStore.activeLibrary
      const libraryId = activeLibrary?.id ?? null
      const libraryName = activeLibrary?.name ?? ''
      if (!activeWorkspace || !activeLibrary) {
        this.uploadLoading = false
        this.error = 'Active workspace and library are required before uploading documents'
        return
      }
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
      this.providerFailureDetail = null
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
      const libraryId = useShellStore().activeLibrary?.id ?? null
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
      const libraryId = useShellStore().activeLibrary?.id ?? null
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
      const libraryId = useShellStore().activeLibrary?.id ?? null
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
      const libraryId = shellStore.activeLibrary?.id ?? null
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
      const libraryId = shellStore.activeLibrary?.id ?? null
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
