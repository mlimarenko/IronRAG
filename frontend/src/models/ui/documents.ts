import type { GraphCanonicalSummary } from './graph'

export type DocumentStatus =
  | 'queued'
  | 'processing'
  | 'ready'
  | 'ready_no_graph'
  | 'failed'

export type DocumentDisplayStatus = 'in_progress' | 'ready' | 'failed'

export type DocumentReadabilityState =
  | 'unreadable'
  | 'readable_active'
  | 'readable_stale'

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

export type DocumentQueueWaitingReason =
  | 'ordinary_backlog'
  | 'isolated_capacity_wait'
  | 'blocked'
  | 'degraded'

export type DocumentCollectionProgressState =
  | 'live_in_flight'
  | 'settling'
  | 'fully_settled'
  | 'failed_with_residual_work'

export type DocumentCollectionTerminalState =
  | 'live_in_flight'
  | 'fully_settled'
  | 'failed_with_residual_work'

export type DocumentCollectionResidualReason =
  | 'graph_write_contention'
  | 'graph_persistence_integrity'
  | 'graph_state_refresh_failed'
  | 'provider_failure'
  | 'diagnostics_unavailable'
  | 'upload_limit_exceeded'
  | 'unknown'

export type DocumentGraphProgressCadence = 'fast' | 'watch' | 'calm'

export type DocumentGraphWriteHealth =
  | 'healthy'
  | 'retrying_contention'
  | 'degraded'
  | 'failed'

export type DocumentCollectionWarningKind =
  | 'ordinary_backlog'
  | 'isolated_capacity_wait'
  | 'in_flight_accounting'
  | 'missing_accounting'
  | 'liveness_loss'
  | 'failed_work'
  | 'degraded_extraction'

export type DocumentMutationStatus = 'accepted' | 'reconciling' | 'completed' | 'failed'
export type DocumentMutationImpactScopeStatus =
  | 'pending'
  | 'targeted'
  | 'fallback_broad'
  | 'completed'
  | 'failed'
export type DocumentMutationImpactScopeConfidence = 'high' | 'medium' | 'low'
export type DocumentExtractionRecoveryStatus =
  | 'clean'
  | 'recovered'
  | 'partial'
  | 'failed'

export type DocumentProviderFailureClass =
  | 'internal_request_invalid'
  | 'upstream_timeout'
  | 'upstream_rejection'
  | 'invalid_model_output'
  | 'recovered_after_retry'
  | 'unknown'

export interface DocumentMutationAccepted {
  accepted: boolean
  operation: string
  trackId: string | null
  revisionId: string | null
  mutationId: string | null
  attemptNo: number | null
}

export interface CanonicalDocumentIdentity {
  id: string
  workspaceId: string
  libraryId: string
  externalKey: string
  documentState: string
  createdAt: string
}

export interface CanonicalDocumentHead {
  documentId: string
  activeRevisionId: string | null
  readableRevisionId: string | null
  latestMutationId: string | null
  latestSuccessfulAttemptId: string | null
  headUpdatedAt: string
}

export interface CanonicalDocumentRevision {
  id: string
  documentId: string
  workspaceId: string
  libraryId: string
  revisionNumber: number
  parentRevisionId: string | null
  contentSourceKind: string
  checksum: string
  mimeType: string
  byteSize: number
  title: string | null
  languageCode: string | null
  sourceUri: string | null
  storageKey: string | null
  createdByPrincipalId: string | null
  createdAt: string
}

export interface CanonicalDocumentMutation {
  id: string
  workspaceId: string
  libraryId: string
  operationKind: string
  mutationState: string
  requestedAt: string
  completedAt: string | null
  requestedByPrincipalId: string | null
  requestSurface: string
  idempotencyKey: string | null
  failureCode: string | null
  conflictCode: string | null
}

export interface CanonicalDocumentMutationItem {
  id: string
  mutationId: string
  documentId: string | null
  baseRevisionId: string | null
  resultRevisionId: string | null
  itemState: string
  message: string | null
}

export interface CanonicalIngestJob {
  id: string
  workspaceId: string
  libraryId: string
  mutationId: string | null
  connectorId: string | null
  jobKind: string
  queueState: string
  priority: number
  dedupeKey: string | null
  queuedAt: string
  availableAt: string
  completedAt: string | null
}

export interface CanonicalIngestAttempt {
  id: string
  jobId: string
  attemptNumber: number
  workerPrincipalId: string | null
  leaseToken: string | null
  attemptState: string
  currentStage: string | null
  startedAt: string
  heartbeatAt: string | null
  finishedAt: string | null
  failureClass: string | null
  failureCode: string | null
  retryable: boolean
}

export interface CanonicalIngestStageEvent {
  id: string
  attemptId: string
  stageName: string
  stageState: string
  ordinal: number
  message: string | null
  detailsJson: Record<string, unknown>
  recordedAt: string
}

export interface DocumentCanonicalState {
  document: CanonicalDocumentIdentity
  head: CanonicalDocumentHead | null
  activeRevision: CanonicalDocumentRevision | null
  readableRevision: CanonicalDocumentRevision | null
  latestMutation: CanonicalDocumentMutation | null
  latestMutationItems: CanonicalDocumentMutationItem[]
  latestJob: CanonicalIngestJob | null
  latestAttempt: CanonicalIngestAttempt | null
  latestAttemptStages: CanonicalIngestStageEvent[]
}

export interface DocumentKnowledgeReadiness {
  revisionId: string | null
  revisionNo: number | null
  revisionKind: string | null
  textState: string
  vectorState: string
  graphState: string
  textReadableAt: string | null
  vectorReadyAt: string | null
  graphReadyAt: string | null
}

export interface DocumentExtractionRecovery {
  status: DocumentExtractionRecoveryStatus
  parserRepairApplied: boolean
  secondPassApplied: boolean
  warning: string | null
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
}

export interface DocumentMutationState {
  kind: string | null
  status: DocumentMutationStatus | null
  warning: string | null
}

export interface DocumentMutationImpactScopeSummary {
  scopeStatus: DocumentMutationImpactScopeStatus
  confidenceStatus: DocumentMutationImpactScopeConfidence
  affectedNodeCount: number
  affectedRelationshipCount: number
  fallbackReason: string | null
}

export interface DocumentRowSummary {
  id: string
  fileName: string
  fileType: string
  fileSizeLabel: string
  uploadedAt: string
  status: DocumentStatus
  statusLabel: string
  activityLabel: string
  mutationLabel: string | null
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
  recovery: DocumentExtractionRecovery | null
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

export interface DocumentTerminalOutcomeSummary {
  terminalState: DocumentCollectionTerminalState
  residualReason: DocumentCollectionResidualReason | null
  queuedCount: number
  processingCount: number
  pendingGraphCount: number
  failedDocumentCount: number
  settledAt: string | null
  lastTransitionAt: string | null
}

export interface DocumentGraphHealthSummary {
  writeHealth: DocumentGraphWriteHealth
  activeWriteCount: number
  retryingWriteCount: number
  failedWriteCount: number
  pendingNodeWriteCount: number
  pendingEdgeWriteCount: number
  lastFailureKind: string | null
  lastFailureAt: string | null
  isRuntimeReadable: boolean
  snapshotAt: string
}

export interface DocumentProviderFailureSummary {
  failureClass: DocumentProviderFailureClass
  providerKind: string | null
  modelName: string | null
  requestShapeKey: string | null
  requestSizeBytes: number | null
  upstreamStatus: string | null
  elapsedMs: number | null
  retryDecision: string | null
  usageVisible: boolean
}

export interface DocumentsWorkspacePrimarySummary {
  progressLabel: string
  spendLabel: string
  backlogLabel: string
  terminalState: string
}

export interface DocumentsWorkspaceDiagnosticChip {
  kind: string
  label: string
  value: string
}

export interface DocumentsWorkspaceNotice {
  kind: string
  title: string
  message: string
}

export interface DocumentsWorkspaceSummary {
  primarySummary: DocumentsWorkspacePrimarySummary
  secondaryDiagnostics: DocumentsWorkspaceDiagnosticChip[]
  degradedNotices: DocumentsWorkspaceNotice[]
  informationalNotices: DocumentsWorkspaceNotice[]
  tableDocumentCount: number
  activeFilterCount: number
  highlightedStatus: string | null
}

export interface DocumentDetail {
  id: string
  logicalDocumentId: string | null
  readabilityState: DocumentReadabilityState
  activeRevisionId: string | null
  readableRevisionId: string | null
  readableRevisionNo: number | null
  readableRevisionKind: string | null
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
  failureClass: string | null
  operatorAction: string | null
  summary: string
  graphNodeId: string | null
  canonicalSummaryPreview: GraphCanonicalSummary | null
  canDownloadText: boolean
  canAppend: boolean
  canReplace: boolean
  canRemove: boolean
  canRetry: boolean
  detailAvailable: boolean
  reconciliationScope: DocumentMutationImpactScopeSummary | null
  providerFailure: DocumentProviderFailureSummary | null
  graphThroughput: DocumentGraphThroughputSummary | null
  knowledgeReadiness: DocumentKnowledgeReadiness | null
  extractedStats: DocumentExtractedStats
  graphStats: DocumentGraphStats
  collectionDiagnostics: DocumentCollectionDiagnostics | null
  revisionHistory: DocumentRevisionHistoryItem[]
  processingHistory: DocumentHistoryItem[]
  attempts: DocumentAttemptGroup[]
  canonical: DocumentCanonicalState
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
  workspace: DocumentsWorkspaceSummary | null
  rows: DocumentRowSummary[]
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
  queueIsolation: DocumentQueueIsolationSummary | null
  graphThroughput: DocumentCollectionGraphThroughputSummary | null
  settlement: DocumentCollectionSettlementSummary | null
  terminalOutcome: DocumentTerminalOutcomeSummary | null
  graphHealth: DocumentGraphHealthSummary | null
  warnings: DocumentCollectionWarning[]
  perStage: DocumentCollectionStageDiagnostics[]
  perFormat: DocumentCollectionFormatDiagnostics[]
}

export interface DocumentGraphThroughputSummary {
  trackedDocumentCount?: number | null
  activeDocumentCount?: number | null
  processedChunks: number
  totalChunks: number
  progressPercent: number | null
  providerCallCount: number
  resumedChunkCount: number
  resumeHitCount: number
  replayedChunkCount: number
  duplicateWorkRatio: number | null
  maxDowngradeLevel: number
  avgCallElapsedMs: number | null
  avgChunkElapsedMs: number | null
  avgCharsPerSecond: number | null
  avgTokensPerSecond: number | null
  lastProviderCallAt: string | null
  lastCheckpointAt: string
  lastCheckpointElapsedMs: number
  nextCheckpointEtaMs: number | null
  pressureKind: 'steady' | 'elevated' | 'high' | null
  cadence: DocumentGraphProgressCadence
  recommendedPollIntervalMs: number
  bottleneckRank: number | null
}

export interface DocumentCollectionGraphThroughputSummary
  extends DocumentGraphThroughputSummary {
  trackedDocumentCount: number
  activeDocumentCount: number
}

export interface DocumentQueueIsolationSummary {
  waitingReason: DocumentQueueWaitingReason
  queuedCount: number
  processingCount: number
  isolatedCapacityCount: number
  availableCapacityCount: number
  lastClaimedAt: string | null
  lastProgressAt: string | null
}

export interface DocumentCollectionSettlementSummary {
  progressState: DocumentCollectionProgressState
  liveTotalEstimatedCost: number | null
  settledTotalEstimatedCost: number | null
  missingTotalEstimatedCost: number | null
  currency: string | null
  isFullySettled: boolean
  settledAt: string | null
}

export interface DocumentCollectionWarning {
  warningKind: DocumentCollectionWarningKind
  warningScope: 'library' | 'collection' | 'document' | 'stage'
  warningMessage: string
  isDegraded: boolean
}

export interface UploadDocumentsResponse {
  acceptedRows: DocumentRowSummary[]
  rejectedFiles: DocumentUploadFailure[]
}

export interface UploadRejectionDetails {
  fileName: string | null
  rejectionKind: string | null
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
  rejectionKind: string | null
  detectedFormat: string | null
  mimeType: string | null
  fileSizeBytes: number | null
  uploadLimitMb: number | null
  rejectionCause: string | null
  operatorAction: string | null
}

export interface DocumentInspectorState {
  documentId: string | null
  loading: boolean
  error: string | null
  detail: DocumentDetail | null
}

export interface DocumentsFilterState {
  searchQuery: string
  statusFilter: DocumentDisplayStatus | ''
  selectedFileTypes: string[]
  sortField: 'uploadedAt' | 'fileName'
  sortDirection: 'asc' | 'desc'
}

export interface DocumentsWorkspaceSurface {
  acceptedFormats: string[]
  maxSizeMb: number
  loading: boolean
  error: string | null
  counters: DocumentSummaryCounters
  rows: DocumentRowSummary[]
  filters: DocumentsFilterState
  inspector: DocumentInspectorState
  uploadInProgress: boolean
  uploadFailures: DocumentUploadFailure[]
  uploadQueue: DocumentRowSummary[]
  selectedDocumentId: string | null
}
