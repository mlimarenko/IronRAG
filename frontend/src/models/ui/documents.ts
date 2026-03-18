export type DocumentStatus =
  | 'queued'
  | 'processing'
  | 'ready'
  | 'ready_no_graph'
  | 'failed'

export type DocumentActivityStatus =
  | 'queued'
  | 'active'
  | 'blocked'
  | 'retrying'
  | 'stalled'
  | 'ready'
  | 'failed'

export type DocumentAccountingStatus =
  | 'priced'
  | 'partial'
  | 'unpriced'
  | 'in_flight_unsettled'

export type DocumentMutationStatus = 'accepted' | 'reconciling' | 'completed' | 'failed'

export interface DocumentMutationAccepted {
  accepted: boolean
  operation: string
  trackId: string | null
  revisionId: string | null
  mutationId: string | null
  attemptNo: number | null
}

export interface DocumentSummaryCounters {
  queued: number
  processing: number
  ready: number
  readyNoGraph: number
  failed: number
}

export interface DocumentFilterValues {
  statuses: DocumentStatus[]
  fileTypes: string[]
  accountingStatuses: DocumentAccountingStatus[]
  mutationStatuses: DocumentMutationStatus[]
}

export interface DocumentMutationState {
  kind: string | null
  status: DocumentMutationStatus | null
  warning: string | null
}

export interface DocumentRow {
  id: string
  logicalDocumentId: string | null
  fileName: string
  fileType: string
  fileSizeLabel: string
  uploadedAt: string
  libraryName: string
  stage: string
  status: DocumentStatus
  progressPercent: number | null
  activityStatus: DocumentActivityStatus
  lastActivityAt: string | null
  stalledReason: string | null
  chunkCount: number | null
  graphNodeCount: number | null
  graphEdgeCount: number | null
  activeRevisionNo: number | null
  activeRevisionKind: string | null
  latestAttemptNo: number
  accountingStatus: DocumentAccountingStatus
  totalEstimatedCost: number | null
  settledEstimatedCost: number | null
  inFlightEstimatedCost: number | null
  currency: string | null
  inFlightStageCount: number
  missingStageCount: number
  partialHistory: boolean
  partialHistoryReason: string | null
  mutation: DocumentMutationState
  canRetry: boolean
  canAppend: boolean
  canReplace: boolean
  canRemove: boolean
  detailAvailable: boolean
}

export interface DocumentHistoryItem {
  attemptNo: number
  status: string
  stage: string
  errorMessage: string | null
  startedAt: string
  finishedAt: string | null
}

export interface DocumentExtractedStats {
  chunkCount: number | null
  documentId: string | null
  checksum: string | null
  pageCount: number | null
  extractionKind: string | null
  previewText: string | null
  previewTruncated: boolean
  warningCount: number
  normalizationStatus: string
  ocrSource: string | null
  warnings: string[]
}

export interface DocumentGraphStats {
  nodeCount: number
  edgeCount: number
  evidenceCount: number
}

export interface DocumentRevisionHistoryItem {
  id: string
  revisionNo: number
  revisionKind: string
  status: string
  sourceFileName: string
  appendedTextExcerpt: string | null
  acceptedAt: string
  activatedAt: string | null
  supersededAt: string | null
  isActive: boolean
}

export interface DocumentStageAccountingItem {
  accountingScope: 'stage_rollup' | 'provider_call' | 'missing'
  pricingStatus: string
  usageEventId: string | null
  costLedgerId: string | null
  pricingCatalogEntryId: string | null
  estimatedCost: number | null
  settledEstimatedCost: number | null
  inFlightEstimatedCost: number | null
  currency: string | null
  attributionSource: 'stage_native' | 'reconciled' | null
}

export interface DocumentStageBenchmarkItem {
  stage: string
  status: string
  message: string | null
  providerKind: string | null
  modelName: string | null
  startedAt: string
  finishedAt: string | null
  elapsedMs: number | null
  accounting: DocumentStageAccountingItem | null
}

export interface DocumentAttemptSummary {
  totalEstimatedCost: number | null
  settledEstimatedCost: number | null
  inFlightEstimatedCost: number | null
  currency: string | null
  pricedStageCount: number
  unpricedStageCount: number
  inFlightStageCount: number
  missingStageCount: number
  accountingStatus: DocumentAccountingStatus
}

export interface DocumentAttemptGroup {
  attemptNo: number
  revisionNo: number | null
  revisionId: string | null
  attemptKind: string | null
  status: string
  activityStatus: DocumentActivityStatus
  lastActivityAt: string | null
  queueElapsedMs: number | null
  totalElapsedMs: number | null
  startedAt: string | null
  finishedAt: string | null
  partialHistory: boolean
  partialHistoryReason: string | null
  summary: DocumentAttemptSummary
  benchmarks: DocumentStageBenchmarkItem[]
}

export interface DocumentDetail {
  id: string
  logicalDocumentId: string | null
  fileName: string
  fileType: string
  fileSizeLabel: string
  uploadedAt: string
  libraryName: string
  stage: string
  status: DocumentStatus
  progressPercent: number | null
  activityStatus: DocumentActivityStatus
  lastActivityAt: string | null
  stalledReason: string | null
  activeRevisionNo: number | null
  activeRevisionKind: string | null
  activeRevisionStatus: string | null
  latestAttemptNo: number
  accountingStatus: DocumentAccountingStatus
  totalEstimatedCost: number | null
  settledEstimatedCost: number | null
  inFlightEstimatedCost: number | null
  currency: string | null
  inFlightStageCount: number
  missingStageCount: number
  partialHistory: boolean
  partialHistoryReason: string | null
  mutation: DocumentMutationState
  requestedBy: string | null
  errorMessage: string | null
  summary: string
  graphNodeId: string | null
  canDownloadText: boolean
  canAppend: boolean
  canReplace: boolean
  canRemove: boolean
  extractedStats: DocumentExtractedStats
  graphStats: DocumentGraphStats
  revisionHistory: DocumentRevisionHistoryItem[]
  processingHistory: DocumentHistoryItem[]
  attempts: DocumentAttemptGroup[]
}

export interface DocumentsSurfaceResponse {
  acceptedFormats: string[]
  maxSizeMb: number
  graphStatus: 'empty' | 'building' | 'ready' | 'partial' | 'failed' | 'stale'
  graphWarning: string | null
  rebuildBacklogCount: number
  counters: DocumentSummaryCounters
  filters: DocumentFilterValues
  accounting: DocumentCollectionAccountingSummary
  diagnostics: DocumentCollectionDiagnostics
  rows: DocumentRow[]
}

export interface DocumentCollectionAccountingSummary {
  totalEstimatedCost: number | null
  settledEstimatedCost: number | null
  inFlightEstimatedCost: number | null
  currency: string | null
  promptTokens: number
  completionTokens: number
  totalTokens: number
  pricedStageCount: number
  unpricedStageCount: number
  inFlightStageCount: number
  missingStageCount: number
  accountingStatus: DocumentAccountingStatus
}

export interface DocumentCollectionProgressCounters {
  accepted: number
  contentExtracted: number
  chunked: number
  embedded: number
  extractingGraph: number
  graphReady: number
  ready: number
  failed: number
}

export interface DocumentCollectionStageDiagnostics {
  stage: string
  activeCount: number
  completedCount: number
  failedCount: number
  avgElapsedMs: number | null
  maxElapsedMs: number | null
  totalEstimatedCost: number | null
  settledEstimatedCost: number | null
  inFlightEstimatedCost: number | null
  currency: string | null
  promptTokens: number
  completionTokens: number
  totalTokens: number
  accountingStatus: DocumentAccountingStatus
}

export interface DocumentCollectionFormatDiagnostics {
  fileType: string
  documentCount: number
  queuedCount: number
  processingCount: number
  readyCount: number
  readyNoGraphCount: number
  failedCount: number
  contentExtractedCount: number
  chunkedCount: number
  embeddedCount: number
  extractingGraphCount: number
  graphReadyCount: number
  avgQueueElapsedMs: number | null
  maxQueueElapsedMs: number | null
  avgTotalElapsedMs: number | null
  maxTotalElapsedMs: number | null
  bottleneckStage: string | null
  bottleneckAvgElapsedMs: number | null
  bottleneckMaxElapsedMs: number | null
  totalEstimatedCost: number | null
  settledEstimatedCost: number | null
  inFlightEstimatedCost: number | null
  currency: string | null
  promptTokens: number
  completionTokens: number
  totalTokens: number
  accountingStatus: DocumentAccountingStatus
}

export interface DocumentCollectionDiagnostics {
  progress: DocumentCollectionProgressCounters
  queueBacklogCount: number
  processingBacklogCount: number
  activeBacklogCount: number
  perStage: DocumentCollectionStageDiagnostics[]
  perFormat: DocumentCollectionFormatDiagnostics[]
}

export interface UploadDocumentsResponse {
  acceptedRows: DocumentRow[]
}

export interface UploadRejectionDetails {
  fileName: string | null
  detectedFormat: string | null
  mimeType: string | null
  fileSizeBytes: number | null
  uploadLimitMb: number | null
  rejectionCause: string | null
  operatorAction: string | null
}

export interface DocumentUploadFailure {
  fileName: string
  message: string
  errorKind: string | null
  detectedFormat: string | null
  mimeType: string | null
  fileSizeBytes: number | null
  uploadLimitMb: number | null
  rejectionCause: string | null
  operatorAction: string | null
}
