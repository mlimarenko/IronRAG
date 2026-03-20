import type {
  CanonicalDocumentHead,
  CanonicalDocumentIdentity,
  CanonicalDocumentMutation,
  CanonicalDocumentMutationItem,
  CanonicalDocumentRevision,
  CanonicalIngestAttempt,
  CanonicalIngestJob,
  CanonicalIngestStageEvent,
  DocumentAccountingStatus,
  DocumentActivityStatus,
  DocumentAttemptGroup,
  DocumentAttemptSummary,
  DocumentCollectionAccountingSummary,
  DocumentCollectionDiagnostics,
  DocumentCollectionFormatDiagnostics,
  DocumentCollectionGraphThroughputSummary,
  DocumentCollectionProgressCounters,
  DocumentCollectionSettlementSummary,
  DocumentCollectionStageDiagnostics,
  DocumentCollectionWarning,
  DocumentDetail,
  DocumentFilterValues,
  DocumentGraphHealthSummary,
  DocumentGraphThroughputSummary,
  DocumentGraphStats,
  DocumentMutationAccepted,
  DocumentMutationImpactScopeSummary,
  DocumentMutationState,
  DocumentProviderFailureSummary,
  DocumentQueueIsolationSummary,
  DocumentQueueWaitingReason,
  DocumentRevisionHistoryItem,
  DocumentRow,
  DocumentStatus,
  DocumentSummaryCounters,
  DocumentsWorkspaceDiagnosticChip,
  DocumentsWorkspaceNotice,
  DocumentsWorkspacePrimarySummary,
  DocumentsWorkspaceSummary,
  DocumentsSurfaceResponse,
  DocumentUploadFailure,
  UploadDocumentsResponse,
  UploadRejectionDetails,
} from 'src/models/ui/documents'
import type { GraphCanonicalSummary } from 'src/models/ui/graph'
import { useShellStore } from 'src/stores/shell'
import { ApiClientError, apiHttp, unwrap } from './http'

interface RawContentDocument {
  id: string
  workspace_id: string
  library_id: string
  external_key: string
  document_state: string
  created_at: string
}

interface RawContentDocumentHead {
  document_id: string
  active_revision_id: string | null
  readable_revision_id: string | null
  latest_mutation_id: string | null
  latest_successful_attempt_id: string | null
  head_updated_at: string
}

interface RawContentRevision {
  id: string
  document_id: string
  workspace_id: string
  library_id: string
  revision_number: number
  parent_revision_id: string | null
  content_source_kind: string
  checksum: string
  mime_type: string
  byte_size: number
  title: string | null
  language_code: string | null
  source_uri: string | null
  storage_key: string | null
  created_by_principal_id: string | null
  created_at: string
}

interface RawContentMutation {
  id: string
  workspace_id: string
  library_id: string
  operation_kind: string
  mutation_state: string
  requested_at: string
  completed_at: string | null
  requested_by_principal_id: string | null
  request_surface: string
  idempotency_key: string | null
  failure_code: string | null
  conflict_code: string | null
}

interface RawContentMutationItem {
  id: string
  mutation_id: string
  document_id: string | null
  base_revision_id: string | null
  result_revision_id: string | null
  item_state: string
  message: string | null
}

interface RawContentDocumentDetailResponse {
  document: RawContentDocument
  head: RawContentDocumentHead | null
  active_revision: RawContentRevision | null
}

interface RawContentMutationDetailResponse {
  mutation: RawContentMutation
  items: RawContentMutationItem[]
  job_id: string | null
}

interface RawCreateDocumentResponse {
  document: RawContentDocumentDetailResponse
  mutation: RawContentMutationDetailResponse
}

interface RawIngestJob {
  id: string
  workspace_id: string
  library_id: string
  mutation_id: string | null
  connector_id: string | null
  job_kind: string
  queue_state: 'queued' | 'leased' | 'completed' | 'failed' | 'canceled' | string
  priority: number
  dedupe_key: string | null
  queued_at: string
  available_at: string
  completed_at: string | null
}

interface RawChunkSummary {
  id: string
  document_id: string
  project_id: string
  ordinal: number
  content: string
  token_count: number | null
}

interface CanonicalLibraryBundle {
  documents: RawContentDocumentDetailResponse[]
  mutations: RawContentMutationDetailResponse[]
  jobs: RawIngestJob[]
}

interface CanonicalDocumentRelation {
  mutations: RawContentMutationDetailResponse[]
  jobs: RawIngestJob[]
}

const DEFAULT_ACCEPTED_FORMATS = ['PDF', 'DOCX', 'TXT', 'MD', 'Images']
const DEFAULT_UPLOAD_LIMIT_MB = 50
const STALLED_ACTIVITY_AFTER_MS = 180_000

function readString(record: Record<string, unknown>, key: string): string | null {
  const value = record[key]
  return typeof value === 'string' ? value : null
}

function readNumber(record: Record<string, unknown>, key: string): number | null {
  const value = record[key]
  return typeof value === 'number' ? value : null
}

function toHex(bytes: ArrayBuffer): string {
  return Array.from(new Uint8Array(bytes))
    .map((byte) => byte.toString(16).padStart(2, '0'))
    .join('')
}

async function sha256Hex(value: string | ArrayBuffer): Promise<string> {
  const bytes = typeof value === 'string' ? new TextEncoder().encode(value) : new Uint8Array(value)
  const digest = await crypto.subtle.digest('SHA-256', bytes)
  return toHex(digest)
}

function compareIsoDates(left: string | null, right: string | null): number {
  const leftTime = left ? Date.parse(left) : Number.NEGATIVE_INFINITY
  const rightTime = right ? Date.parse(right) : Number.NEGATIVE_INFINITY
  return rightTime - leftTime
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

function inferFileType(fileName: string, mimeType: string | null): string {
  const extension = fileName.split('.').pop()?.toLowerCase() ?? ''
  if (mimeType?.startsWith('image/') || ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'svg'].includes(extension)) {
    return 'Image'
  }
  if (mimeType === 'application/pdf' || extension === 'pdf') {
    return 'PDF'
  }
  if (extension === 'docx' || mimeType?.includes('wordprocessingml')) {
    return 'DOCX'
  }
  if (
    ['md', 'markdown', 'txt', 'text', 'log', 'csv', 'json', 'yaml', 'yml', 'xml'].includes(extension) ||
    mimeType?.startsWith('text/')
  ) {
    return 'Text'
  }
  return extension ? extension.toUpperCase() : 'File'
}

function mutationStatusFromState(value: string): 'accepted' | 'reconciling' | 'completed' | 'failed' {
  switch (value) {
    case 'accepted':
      return 'accepted'
    case 'running':
      return 'reconciling'
    case 'applied':
      return 'completed'
    case 'failed':
    case 'conflicted':
    case 'canceled':
      return 'failed'
    default:
      return 'reconciling'
  }
}

function documentStatusFromQueueState(
  queueState: RawIngestJob['queue_state'] | null,
  readableRevisionPresent: boolean,
): DocumentStatus {
  switch (queueState) {
    case 'queued':
    case 'leased':
      return 'processing'
    case 'completed':
      return readableRevisionPresent ? 'ready' : 'ready_no_graph'
    case 'failed':
    case 'canceled':
      return 'failed'
    default:
      return readableRevisionPresent ? 'ready' : 'queued'
  }
}

function activityStatusFromQueueState(
  queueState: RawIngestJob['queue_state'] | null,
  lastActivityAt: string | null,
): DocumentActivityStatus {
  let activityStatus: DocumentActivityStatus
  switch (queueState) {
    case 'queued':
      activityStatus = 'queued'
      break
    case 'leased':
      activityStatus = 'active'
      break
    case 'completed':
      activityStatus = 'ready'
      break
    case 'failed':
    case 'canceled':
      activityStatus = 'failed'
      break
    default:
      activityStatus = 'active'
      break
  }

  if (
    (activityStatus === 'queued' || activityStatus === 'active') &&
    lastActivityAt !== null
  ) {
    const lastSeenAt = Date.parse(lastActivityAt)
    if (!Number.isNaN(lastSeenAt) && Date.now() - lastSeenAt >= STALLED_ACTIVITY_AFTER_MS) {
      return 'stalled'
    }
  }

  return activityStatus
}

function stalledReason(activityStatus: DocumentActivityStatus, lastActivityAt: string | null): string | null {
  if (activityStatus !== 'stalled' || !lastActivityAt) {
    return null
  }
  const lastSeenAt = Date.parse(lastActivityAt)
  if (Number.isNaN(lastSeenAt)) {
    return null
  }
  const elapsedSeconds = Math.max(0, Math.round((Date.now() - lastSeenAt) / 1000))
  return `No visible activity for ${String(elapsedSeconds)}s`
}

function latestActivityAt(job: RawIngestJob | null): string | null {
  if (!job) {
    return null
  }
  return job.completed_at ?? job.available_at ?? job.queued_at
}

function elapsedMs(left: string | null, right: string | null): number | null {
  if (!left || !right) {
    return null
  }
  const leftTime = Date.parse(left)
  const rightTime = Date.parse(right)
  if (Number.isNaN(leftTime) || Number.isNaN(rightTime)) {
    return null
  }
  return Math.max(0, rightTime - leftTime)
}

function buildCanonicalAttempt(job: RawIngestJob): CanonicalIngestAttempt {
  const lastActivity = latestActivityAt(job)
  return {
    id: job.id,
    jobId: job.id,
    attemptNumber: 1,
    workerPrincipalId: null,
    leaseToken: null,
    attemptState:
      job.queue_state === 'completed'
        ? 'succeeded'
        : job.queue_state === 'failed' || job.queue_state === 'canceled'
          ? 'failed'
          : job.queue_state === 'leased'
            ? 'running'
            : 'leased',
    currentStage: job.job_kind,
    startedAt: job.queued_at,
    heartbeatAt: lastActivity,
    finishedAt: job.completed_at,
    failureClass: job.queue_state === 'failed' || job.queue_state === 'canceled' ? 'ingest_failed' : null,
    failureCode: null,
    retryable: job.queue_state === 'failed',
  }
}

function buildCanonicalJob(job: RawIngestJob): CanonicalIngestJob {
  return {
    id: job.id,
    workspaceId: job.workspace_id,
    libraryId: job.library_id,
    mutationId: job.mutation_id,
    connectorId: job.connector_id,
    jobKind: job.job_kind,
    queueState: job.queue_state,
    priority: job.priority,
    dedupeKey: job.dedupe_key,
    queuedAt: job.queued_at,
    availableAt: job.available_at,
    completedAt: job.completed_at,
  }
}

function buildCanonicalMutation(mutation: RawContentMutation): CanonicalDocumentMutation {
  return {
    id: mutation.id,
    workspaceId: mutation.workspace_id,
    libraryId: mutation.library_id,
    operationKind: mutation.operation_kind,
    mutationState: mutation.mutation_state,
    requestedAt: mutation.requested_at,
    completedAt: mutation.completed_at,
    requestedByPrincipalId: mutation.requested_by_principal_id,
    requestSurface: mutation.request_surface,
    idempotencyKey: mutation.idempotency_key,
    failureCode: mutation.failure_code,
    conflictCode: mutation.conflict_code,
  }
}

function buildCanonicalMutationItem(item: RawContentMutationItem): CanonicalDocumentMutationItem {
  return {
    id: item.id,
    mutationId: item.mutation_id,
    documentId: item.document_id,
    baseRevisionId: item.base_revision_id,
    resultRevisionId: item.result_revision_id,
    itemState: item.item_state,
    message: item.message,
  }
}

function buildCanonicalRevision(revision: RawContentRevision): CanonicalDocumentRevision {
  return {
    id: revision.id,
    documentId: revision.document_id,
    workspaceId: revision.workspace_id,
    libraryId: revision.library_id,
    revisionNumber: revision.revision_number,
    parentRevisionId: revision.parent_revision_id,
    contentSourceKind: revision.content_source_kind,
    checksum: revision.checksum,
    mimeType: revision.mime_type,
    byteSize: revision.byte_size,
    title: revision.title,
    languageCode: revision.language_code,
    sourceUri: revision.source_uri,
    storageKey: revision.storage_key,
    createdByPrincipalId: revision.created_by_principal_id,
    createdAt: revision.created_at,
  }
}

function buildCanonicalIdentity(document: RawContentDocument): CanonicalDocumentIdentity {
  return {
    id: document.id,
    workspaceId: document.workspace_id,
    libraryId: document.library_id,
    externalKey: document.external_key,
    documentState: document.document_state,
    createdAt: document.created_at,
  }
}

function buildDocumentFileName(
  document: RawContentDocument,
  activeRevision: RawContentRevision | null,
  revisions: RawContentRevision[],
): string {
  return activeRevision?.title ?? revisions[0]?.title ?? document.external_key
}

function buildDocumentFileType(
  document: RawContentDocument,
  activeRevision: RawContentRevision | null,
  revisions: RawContentRevision[],
): string {
  const reference = activeRevision ?? revisions[0] ?? null
  if (!reference) {
    return inferFileType(document.external_key, null)
  }
  return inferFileType(reference.title ?? document.external_key, reference.mime_type)
}

function buildMutationState(mutation: RawContentMutation | null): DocumentMutationState {
  if (!mutation) {
    return {
      kind: null,
      status: null,
      warning: null,
    }
  }
  return {
    kind: mutation.operation_kind,
    status: mutationStatusFromState(mutation.mutation_state),
    warning: mutation.failure_code ?? mutation.conflict_code,
  }
}

function buildAttemptSummary(job: RawIngestJob, accountingStatus: DocumentAccountingStatus): DocumentAttemptSummary {
  return {
    totalEstimatedCost: null,
    settledEstimatedCost: null,
    inFlightEstimatedCost: null,
    currency: null,
    pricedStageCount: job.queue_state === 'completed' ? 1 : 0,
    unpricedStageCount: job.queue_state === 'completed' ? 0 : 1,
    inFlightStageCount: job.queue_state === 'queued' || job.queue_state === 'leased' ? 1 : 0,
    missingStageCount: 0,
    accountingStatus,
  }
}

function buildAttemptGroup(job: RawIngestJob, revision: RawContentRevision | null): DocumentAttemptGroup {
  const activityStatus = activityStatusFromQueueState(job.queue_state, latestActivityAt(job))
  const summary: DocumentAttemptSummary = buildAttemptSummary(
    job,
    job.queue_state === 'completed' ? 'priced' : 'in_flight_unsettled',
  )
  return {
    attemptNo: 1,
    revisionNo: revision?.revision_number ?? null,
    revisionId: revision?.id ?? null,
    attemptKind: job.job_kind,
    status: job.queue_state,
    activityStatus,
    lastActivityAt: latestActivityAt(job),
    queueElapsedMs: elapsedMs(job.queued_at, job.available_at ?? job.completed_at ?? null),
    totalElapsedMs: elapsedMs(job.queued_at, job.completed_at ?? new Date().toISOString()),
    startedAt: job.queued_at,
    finishedAt: job.completed_at,
    partialHistory: job.queue_state !== 'completed',
    partialHistoryReason: job.queue_state === 'failed' || job.queue_state === 'canceled' ? 'ingest_failed' : null,
    summary,
    benchmarks: [],
  }
}

function buildProcessingHistory(job: RawIngestJob): DocumentDetail['processingHistory'][number] {
  return {
    attemptNo: 1,
    status: job.queue_state,
    stage: job.job_kind,
    errorMessage: job.queue_state === 'failed' || job.queue_state === 'canceled' ? 'Canonical ingest job failed' : null,
    startedAt: job.queued_at,
    finishedAt: job.completed_at,
  }
}

function buildCanonicalState(
  document: RawContentDocument,
  head: RawContentDocumentHead | null,
  activeRevision: RawContentRevision | null,
  readableRevision: RawContentRevision | null,
  mutation: RawContentMutationDetailResponse | null,
  latestJob: RawIngestJob | null,
  revisionItems: RawContentMutationDetailResponse[],
  jobItems: RawIngestJob[],
  stageEvents: CanonicalIngestStageEvent[] = [],
): DocumentDetail['canonical'] {
  const latestMutation = mutation ? buildCanonicalMutation(mutation.mutation) : null
  const latestMutationItems = mutation ? mutation.items.map(buildCanonicalMutationItem) : []
  return {
    document: buildCanonicalIdentity(document),
    head:
      head === null
        ? null
        : {
            documentId: head.document_id,
            activeRevisionId: head.active_revision_id,
            readableRevisionId: head.readable_revision_id,
            latestMutationId: head.latest_mutation_id,
            latestSuccessfulAttemptId: head.latest_successful_attempt_id,
            headUpdatedAt: head.head_updated_at,
          },
    activeRevision: activeRevision ? buildCanonicalRevision(activeRevision) : null,
    readableRevision: readableRevision ? buildCanonicalRevision(readableRevision) : null,
    latestMutation,
    latestMutationItems,
    latestJob: latestJob ? buildCanonicalJob(latestJob) : null,
    latestAttempt: latestJob ? buildCanonicalAttempt(latestJob) : null,
    latestAttemptStages: stageEvents,
  }
}

function buildGraphThroughput(
  activeCount: number,
  totalCount: number,
  completedCount: number,
  pressureKind: 'steady' | 'elevated' | 'high' | null,
): Omit<DocumentGraphThroughputSummary, 'trackedDocumentCount' | 'activeDocumentCount'> {
  const backlog = Math.max(0, activeCount)
  const cadence = backlog > 0 ? 'watch' : 'calm'
  const recommendedPollIntervalMs = backlog > 0 ? 4_000 : 0
  return {
    processedChunks: completedCount,
    totalChunks: totalCount,
    progressPercent: totalCount > 0 ? Math.round((completedCount / totalCount) * 100) : null,
    providerCallCount: 0,
    resumedChunkCount: 0,
    resumeHitCount: 0,
    replayedChunkCount: 0,
    duplicateWorkRatio: null,
    maxDowngradeLevel: pressureKind === 'high' ? 2 : pressureKind === 'elevated' ? 1 : 0,
    avgCallElapsedMs: null,
    avgChunkElapsedMs: null,
    avgCharsPerSecond: null,
    avgTokensPerSecond: null,
    lastProviderCallAt: null,
    lastCheckpointAt: new Date().toISOString(),
    lastCheckpointElapsedMs: 0,
    nextCheckpointEtaMs: backlog > 0 ? recommendedPollIntervalMs : null,
    pressureKind,
    cadence,
    recommendedPollIntervalMs,
    bottleneckRank: backlog > 0 ? 1 : null,
  }
}

function buildCollectionAccounting(
  rows: DocumentRow[],
): DocumentCollectionAccountingSummary {
  const queued = rows.filter((row) => row.status === 'queued').length
  const processing = rows.filter((row) => row.status === 'processing').length
  const ready = rows.filter((row) => row.status === 'ready').length
  const readyNoGraph = rows.filter((row) => row.status === 'ready_no_graph').length
  const failed = rows.filter((row) => row.status === 'failed').length
  const inFlightStageCount = queued + processing
  return {
    totalEstimatedCost: null,
    settledEstimatedCost: null,
    inFlightEstimatedCost: null,
    currency: null,
    promptTokens: 0,
    completionTokens: 0,
    totalTokens: 0,
    pricedStageCount: ready + readyNoGraph,
    unpricedStageCount: queued + processing + failed,
    inFlightStageCount,
    missingStageCount: 0,
    accountingStatus: inFlightStageCount > 0 ? 'in_flight_unsettled' : 'priced',
  }
}

function buildCollectionDiagnostics(rows: DocumentRow[]): DocumentCollectionDiagnostics {
  const queued = rows.filter((row) => row.status === 'queued').length
  const processing = rows.filter((row) => row.status === 'processing').length
  const ready = rows.filter((row) => row.status === 'ready').length
  const readyNoGraph = rows.filter((row) => row.status === 'ready_no_graph').length
  const failed = rows.filter((row) => row.status === 'failed').length
  const activeBacklogCount = queued + processing
  const progress: DocumentCollectionProgressCounters = {
    accepted: rows.length,
    contentExtracted: ready + readyNoGraph,
    chunked: ready + readyNoGraph,
    embedded: ready,
    extractingGraph: processing,
    graphReady: ready,
    ready,
    failed,
  }
  const warnings: DocumentCollectionWarning[] = []
  if (activeBacklogCount > 0) {
    warnings.push({
      warningKind: 'ordinary_backlog',
      warningScope: 'collection',
      warningMessage: `${String(activeBacklogCount)} canonical document(s) are still ingesting.`,
      isDegraded: false,
    })
  }
  if (failed > 0) {
    warnings.push({
      warningKind: 'failed_work',
      warningScope: 'collection',
      warningMessage: `${String(failed)} canonical document(s) failed ingestion.`,
      isDegraded: true,
    })
  }
  const projectionHealth: DocumentGraphHealthSummary['projectionHealth'] =
    failed > 0 ? 'failed' : activeBacklogCount > 0 ? 'retrying_contention' : 'healthy'
  const graphHealth: DocumentGraphHealthSummary = {
    projectionHealth,
    activeProjectionCount: ready,
    retryingProjectionCount: processing,
    failedProjectionCount: failed,
    pendingNodeWriteCount: activeBacklogCount,
    pendingEdgeWriteCount: activeBacklogCount,
    lastFailureKind: failed > 0 ? 'canonical_ingest_failed' : null,
    lastFailureAt: failed > 0 ? new Date().toISOString() : null,
    isRuntimeReadable: failed === 0 && activeBacklogCount === 0 && ready > 0,
    snapshotAt: new Date().toISOString(),
  }
  const settlement: DocumentCollectionSettlementSummary = {
    progressState:
      activeBacklogCount > 0
        ? 'live_in_flight'
        : failed > 0
          ? 'failed_with_residual_work'
          : 'fully_settled',
    liveTotalEstimatedCost: null,
    settledTotalEstimatedCost: null,
    missingTotalEstimatedCost: null,
    currency: null,
    isFullySettled: activeBacklogCount === 0 && failed === 0,
    settledAt: activeBacklogCount === 0 ? new Date().toISOString() : null,
  }
  const terminalOutcome = {
    terminalState:
      activeBacklogCount > 0
        ? 'live_in_flight'
        : failed > 0
          ? 'failed_with_residual_work'
          : 'fully_settled',
    residualReason: failed > 0 ? 'unknown' : null,
    queuedCount: queued,
    processingCount: processing,
    pendingGraphCount: readyNoGraph,
    failedDocumentCount: failed,
    settledAt: settlement.settledAt,
    lastTransitionAt:
      rows.length > 0
        ? rows
            .map((row) => row.lastActivityAt)
            .filter((value): value is string => Boolean(value))
            .sort(compareIsoDates)
            [0] ?? null
        : null,
  } satisfies NonNullable<DocumentCollectionDiagnostics['terminalOutcome']>

  const graphThroughput: DocumentCollectionGraphThroughputSummary = {
    trackedDocumentCount: rows.length,
    activeDocumentCount: activeBacklogCount,
    ...buildGraphThroughput(
      activeBacklogCount,
      rows.length,
      ready + readyNoGraph,
      failed > 0 ? 'high' : activeBacklogCount > 0 ? 'elevated' : null,
    ),
  }

  return {
    progress,
    queueBacklogCount: queued,
    processingBacklogCount: processing,
    activeBacklogCount,
    queueIsolation: null,
    graphThroughput,
    settlement,
    terminalOutcome,
    graphHealth,
    warnings,
    perStage: [],
    perFormat: [],
  }
}

function buildWorkspaceSummary(
  rows: DocumentRow[],
  diagnostics: DocumentCollectionDiagnostics,
): DocumentsWorkspaceSummary {
  const queued = rows.filter((row) => row.status === 'queued').length
  const processing = rows.filter((row) => row.status === 'processing').length
  const ready = rows.filter((row) => row.status === 'ready').length
  const readyNoGraph = rows.filter((row) => row.status === 'ready_no_graph').length
  const failed = rows.filter((row) => row.status === 'failed').length
  const backlogCount = queued + processing
  const progressCount = ready + readyNoGraph + failed
  const primarySummary: DocumentsWorkspacePrimarySummary = {
    progressLabel: `${String(progressCount)} / ${String(rows.length)}`,
    spendLabel:
      backlogCount > 0
        ? 'In flight'
        : failed > 0
          ? 'Residual work'
          : 'Settled',
    backlogLabel:
      backlogCount > 0 ? `${String(backlogCount)} pending` : 'Clear',
    terminalState: diagnostics.terminalOutcome?.terminalState ?? 'live_in_flight',
  }
  const secondaryDiagnostics: DocumentsWorkspaceDiagnosticChip[] = [
    {
      kind: 'documents',
      label: 'Documents',
      value: String(rows.length),
    },
    {
      kind: 'queue',
      label: 'Queued',
      value: String(queued),
    },
    {
      kind: 'processing',
      label: 'Processing',
      value: String(processing),
    },
    {
      kind: 'failed',
      label: 'Failed',
      value: String(failed),
    },
  ]
  const degradedNotices: DocumentsWorkspaceNotice[] = []
  const informationalNotices: DocumentsWorkspaceNotice[] = []

  if (backlogCount > 0) {
    informationalNotices.push({
      kind: 'ordinary_backlog',
      title: 'Canonical ingestion is active',
      message: `${String(backlogCount)} document(s) are still processing in the canonical pipeline.`,
    })
  }

  if (failed > 0) {
    degradedNotices.push({
      kind: 'failed_work',
      title: 'Canonical ingestion failures',
      message: `${String(failed)} document(s) failed in the canonical pipeline.`,
    })
  }

  return {
    primarySummary,
    secondaryDiagnostics,
    degradedNotices,
    informationalNotices,
    tableDocumentCount: rows.length,
    activeFilterCount: 0,
    highlightedStatus:
      failed > 0 ? 'failed' : backlogCount > 0 ? 'processing' : rows.length > 0 ? 'ready' : null,
  }
}

function collectMutationsForDocument(
  documentId: string,
  mutations: RawContentMutationDetailResponse[],
): RawContentMutationDetailResponse[] {
  return mutations
    .filter((mutation) => mutation.items.some((item) => item.document_id === documentId))
    .sort((left, right) => compareIsoDates(left.mutation.requested_at, right.mutation.requested_at))
}

function selectLatestMutation(
  document: RawContentDocumentDetailResponse,
  relatedMutations: RawContentMutationDetailResponse[],
): RawContentMutationDetailResponse | null {
  if (document.head?.latest_mutation_id) {
    const headMutation = relatedMutations.find(
      (mutation) => mutation.mutation.id === document.head?.latest_mutation_id,
    )
    if (headMutation) {
      return headMutation
    }
  }
  return relatedMutations[0] ?? null
}

function selectLatestJob(
  latestMutation: RawContentMutationDetailResponse | null,
  relatedJobs: RawIngestJob[],
): RawIngestJob | null {
  if (latestMutation?.job_id) {
    const job = relatedJobs.find((item) => item.id === latestMutation.job_id)
    if (job) {
      return job
    }
  }
  return relatedJobs[0] ?? null
}

function mapSurfaceRow(
  document: RawContentDocumentDetailResponse,
  relatedMutations: RawContentMutationDetailResponse[],
  relatedJobs: RawIngestJob[],
  libraryName: string,
): DocumentRow {
  const revisions = document.active_revision ? [document.active_revision] : []
  const latestMutation = selectLatestMutation(document, relatedMutations)
  const latestJob = selectLatestJob(latestMutation, relatedJobs)
  const readableRevisionPresent = document.head?.readable_revision_id !== null
  const documentStatus = documentStatusFromQueueState(latestJob?.queue_state ?? null, readableRevisionPresent)
  const activityStatus = activityStatusFromQueueState(latestJob?.queue_state ?? null, latestActivityAt(latestJob))
  const activeRevision = document.active_revision
  const fileName = buildDocumentFileName(document.document, activeRevision, revisions)
  const fileType = buildDocumentFileType(document.document, activeRevision, revisions)
  const mutationState = buildMutationState(latestMutation?.mutation ?? null)
  const documentHasActivity = Boolean(latestJob || latestMutation)

  return {
    id: document.document.id,
    logicalDocumentId: document.document.id,
    readabilityState:
      documentStatus === 'ready'
        ? 'readable_active'
        : documentStatus === 'ready_no_graph'
          ? 'readable_stale'
          : 'unreadable',
    activeRevisionId: activeRevision?.id ?? null,
    readableRevisionId:
      document.head?.readable_revision_id ?? activeRevision?.id ?? null,
    readableRevisionNo:
      document.head?.readable_revision_id && activeRevision?.id === document.head.readable_revision_id
        ? activeRevision?.revision_number ?? null
        : null,
    fileName,
    fileType,
    fileSizeLabel: activeRevision ? formatFileSizeLabel(activeRevision.byte_size) : '—',
    uploadedAt: document.document.created_at,
    libraryName,
    stage: latestJob?.job_kind ?? activeRevision?.content_source_kind ?? document.document.document_state,
    status: documentStatus,
    progressPercent:
      documentStatus === 'ready'
        ? 100
        : documentStatus === 'ready_no_graph'
          ? 95
          : activityStatus === 'queued'
            ? 0
            : activityStatus === 'active'
              ? 50
              : activityStatus === 'stalled'
                ? 25
                : 100,
    activityStatus,
    lastActivityAt: latestActivityAt(latestJob),
    stalledReason: stalledReason(activityStatus, latestActivityAt(latestJob)),
    chunkCount: null,
    graphNodeCount: activeRevision ? 1 : null,
    graphEdgeCount: 0,
    activeRevisionNo: activeRevision?.revision_number ?? null,
    activeRevisionKind: activeRevision?.content_source_kind ?? null,
    latestAttemptNo: Math.max(relatedJobs.length, documentHasActivity ? 1 : 0),
    accountingStatus:
      documentStatus === 'ready' || documentStatus === 'ready_no_graph'
        ? 'priced'
        : documentStatus === 'failed'
          ? 'partial'
          : 'in_flight_unsettled',
    totalEstimatedCost: null,
    settledEstimatedCost: null,
    inFlightEstimatedCost: null,
    currency: null,
    inFlightStageCount: activityStatus === 'queued' || activityStatus === 'active' ? 1 : 0,
    missingStageCount: 0,
    partialHistory: relatedJobs.length > 1 || documentStatus !== 'ready',
    partialHistoryReason: latestMutation?.mutation.failure_code ?? latestMutation?.mutation.conflict_code ?? null,
    graphThroughput: buildGraphThroughput(
      activityStatus === 'queued' || activityStatus === 'active' ? 1 : 0,
      1,
      documentStatus === 'ready' || documentStatus === 'ready_no_graph' ? 1 : 0,
      activityStatus === 'queued' || activityStatus === 'active' ? 'elevated' : null,
    ),
    mutation: mutationState,
    canRetry:
      latestJob?.queue_state === 'failed' ||
      latestJob?.queue_state === 'canceled' ||
      mutationState.status === 'failed',
    canAppend: document.document.document_state === 'active',
    canReplace: document.document.document_state === 'active',
    canRemove: document.document.document_state === 'active',
    detailAvailable: document.document.document_state === 'active',
    canonical: buildCanonicalState(
      document.document,
      document.head,
      activeRevision,
      activeRevision,
      latestMutation,
      latestJob,
      relatedMutations,
      relatedJobs,
    ),
  }
}

function mapDetailRevisionHistory(
  document: RawContentDocumentDetailResponse,
  revisions: RawContentRevision[],
): DocumentRevisionHistoryItem[] {
  const activeRevisionId = document.head?.active_revision_id ?? document.active_revision?.id ?? null
  return revisions
    .slice()
    .sort((left, right) => left.revision_number - right.revision_number)
    .map((revision) => ({
      id: revision.id,
      revisionNo: revision.revision_number,
      revisionKind: revision.content_source_kind,
      status: revision.id === activeRevisionId ? 'active' : 'superseded',
      sourceFileName: revision.title ?? document.document.external_key,
      appendedTextExcerpt: null,
      acceptedAt: revision.created_at,
      activatedAt: revision.id === activeRevisionId ? revision.created_at : null,
      supersededAt: null,
      isActive: revision.id === activeRevisionId,
    }))
}

function mapChunksToPreview(
  chunks: RawChunkSummary[],
): {
  previewText: string | null
  previewTruncated: boolean
} {
  if (chunks.length === 0) {
    return {
      previewText: null,
      previewTruncated: false,
    }
  }
  const sorted = [...chunks].sort((left, right) => left.ordinal - right.ordinal)
  const previewText = sorted.map((chunk) => chunk.content).join('\n\n')
  const maxPreviewLength = 1_600
  return {
    previewText: previewText.slice(0, maxPreviewLength),
    previewTruncated: previewText.length > maxPreviewLength,
  }
}

function mapDetailProcessingHistory(
  jobs: RawIngestJob[],
): DocumentDetail['processingHistory'] {
  return jobs
    .slice()
    .sort((left, right) => compareIsoDates(left.queued_at, right.queued_at))
    .map(buildProcessingHistory)
}

function mapDetailAttempts(
  jobs: RawIngestJob[],
  revision: RawContentRevision | null,
): DocumentAttemptGroup[] {
  return jobs
    .slice()
    .sort((left, right) => compareIsoDates(left.queued_at, right.queued_at))
    .map((job, index) => ({
      ...buildAttemptGroup(job, revision),
      attemptNo: index + 1,
    }))
}

function mapDocumentDetail(
  document: RawContentDocumentDetailResponse,
  revisions: RawContentRevision[],
  chunks: RawChunkSummary[],
  relatedMutations: RawContentMutationDetailResponse[],
  relatedJobs: RawIngestJob[],
  latestMutation: RawContentMutationDetailResponse | null,
  libraryName: string,
): DocumentDetail {
  const activeRevision = document.active_revision
  const readableRevision =
    document.head?.readable_revision_id
      ? revisions.find((revision) => revision.id === document.head?.readable_revision_id) ?? null
      : activeRevision
  const latestJob = selectLatestJob(latestMutation, relatedJobs)
  const status = documentStatusFromQueueState(latestJob?.queue_state ?? null, Boolean(readableRevision))
  const activityStatus = activityStatusFromQueueState(latestJob?.queue_state ?? null, latestActivityAt(latestJob))
  const canonicalSummary = mapChunksToPreview(chunks)
  const canonicalSummaryPreview: GraphCanonicalSummary | null = canonicalSummary.previewText
    ? {
        text: canonicalSummary.previewText,
        confidenceStatus: canonicalSummary.previewTruncated ? 'partial' : 'strong',
        supportCount: chunks.length,
        warning: canonicalSummary.previewTruncated ? 'Preview truncated from canonical chunks.' : null,
      }
    : null
  const extractionRecovery = null
  const previewChunkCount = chunks.length
  const graphStats: DocumentGraphStats = {
    nodeCount: activeRevision ? 1 : 0,
    edgeCount: 0,
    evidenceCount: previewChunkCount,
  }
  const mutationState = buildMutationState(latestMutation?.mutation ?? null)
  const lastActivity = latestActivityAt(latestJob)
  const attemptGroups = mapDetailAttempts(relatedJobs, activeRevision)
  const processingHistory = mapDetailProcessingHistory(relatedJobs)
  const latestJobForCanonical = latestJob ?? null
  const stageEvents: CanonicalIngestStageEvent[] = []

  return {
    id: document.document.id,
    logicalDocumentId: document.document.id,
    readabilityState:
      status === 'ready'
        ? 'readable_active'
        : status === 'ready_no_graph'
          ? 'readable_stale'
          : 'unreadable',
    activeRevisionId: activeRevision?.id ?? null,
    readableRevisionId: readableRevision?.id ?? null,
    readableRevisionNo: readableRevision?.revision_number ?? null,
    readableRevisionKind: readableRevision?.content_source_kind ?? null,
    fileName: buildDocumentFileName(document.document, activeRevision, revisions),
    fileType: buildDocumentFileType(document.document, activeRevision, revisions),
    fileSizeLabel: activeRevision ? formatFileSizeLabel(activeRevision.byte_size) : '—',
    uploadedAt: document.document.created_at,
    libraryName,
    stage: latestJob?.job_kind ?? activeRevision?.content_source_kind ?? document.document.document_state,
    status,
    progressPercent:
      status === 'ready'
        ? 100
        : status === 'ready_no_graph'
          ? 95
          : activityStatus === 'queued'
            ? 0
            : activityStatus === 'active'
              ? 50
              : activityStatus === 'stalled'
                ? 25
                : 100,
    activityStatus,
    lastActivityAt: lastActivity,
    stalledReason: stalledReason(activityStatus, lastActivity),
    activeRevisionNo: activeRevision?.revision_number ?? null,
    activeRevisionKind: activeRevision?.content_source_kind ?? null,
    activeRevisionStatus: activeRevision ? 'active' : null,
    latestAttemptNo: Math.max(relatedJobs.length, latestJobForCanonical ? 1 : 0),
    accountingStatus:
      status === 'ready' || status === 'ready_no_graph'
        ? 'priced'
        : status === 'failed'
          ? 'partial'
          : 'in_flight_unsettled',
    totalEstimatedCost: null,
    settledEstimatedCost: null,
    inFlightEstimatedCost: null,
    currency: null,
    inFlightStageCount: activityStatus === 'queued' || activityStatus === 'active' ? 1 : 0,
    missingStageCount: 0,
    partialHistory: relatedJobs.length > 1 || status !== 'ready',
    partialHistoryReason: latestMutation?.mutation.failure_code ?? latestMutation?.mutation.conflict_code ?? null,
    mutation: mutationState,
    requestedBy: latestMutation?.mutation.requested_by_principal_id ?? null,
    errorMessage:
      latestMutation?.mutation.failure_code ??
      latestMutation?.mutation.conflict_code ??
      (status === 'failed' ? 'Canonical ingestion failed' : null),
    failureClass:
      latestJob?.queue_state === 'failed' || latestJob?.queue_state === 'canceled'
        ? 'ingest_failed'
        : null,
    operatorAction: latestMutation?.mutation.operation_kind ?? null,
    summary:
      canonicalSummaryPreview?.text ??
      activeRevision?.title ??
      document.document.external_key,
    graphNodeId: activeRevision ? document.document.id : null,
    canonicalSummaryPreview,
    canDownloadText: previewChunkCount > 0,
    canAppend: document.document.document_state === 'active',
    canReplace: document.document.document_state === 'active',
    canRemove: document.document.document_state === 'active',
    canRetry:
      latestJob?.queue_state === 'failed' ||
      latestJob?.queue_state === 'canceled' ||
      mutationState.status === 'failed',
    detailAvailable: document.document.document_state === 'active',
    reconciliationScope: null,
    providerFailure: null,
    graphThroughput: buildGraphThroughput(
      activityStatus === 'queued' || activityStatus === 'active' ? 1 : 0,
      Math.max(1, previewChunkCount),
      previewChunkCount,
      activityStatus === 'queued' || activityStatus === 'active' ? 'elevated' : null,
    ),
    extractedStats: {
      chunkCount: previewChunkCount || null,
      documentId: document.document.id,
      checksum: activeRevision?.checksum ?? null,
      pageCount: null,
      extractionKind: activeRevision?.content_source_kind ?? null,
      previewText: canonicalSummary.previewText,
      previewTruncated: canonicalSummary.previewTruncated,
      warningCount: canonicalSummary.previewTruncated ? 1 : 0,
      normalizationStatus: 'verbatim',
      ocrSource: null,
      recovery: extractionRecovery,
      warnings: [],
    },
    graphStats,
    collectionDiagnostics: null,
    revisionHistory: mapDetailRevisionHistory(document, revisions),
    processingHistory,
    attempts: attemptGroups,
    canonical: buildCanonicalState(
      document.document,
      document.head,
      activeRevision,
      readableRevision,
      latestMutation,
      latestJobForCanonical,
      relatedMutations,
      relatedJobs,
      stageEvents,
    ),
  }
}

function normalizeUploadRejectionDetails(details: unknown): UploadRejectionDetails | null {
  if (!details || typeof details !== 'object') {
    return null
  }
  const record = details as Record<string, unknown>
  return {
    fileName: readString(record, 'fileName'),
    rejectionKind: readString(record, 'rejectionKind'),
    detectedFormat: readString(record, 'detectedFormat'),
    mimeType: readString(record, 'mimeType'),
    fileSizeBytes: readNumber(record, 'fileSizeBytes'),
    uploadLimitMb: readNumber(record, 'uploadLimitMb'),
    rejectionCause: readString(record, 'rejectionCause'),
    operatorAction: readString(record, 'operatorAction'),
  }
}

async function fetchCanonicalBundle(): Promise<CanonicalLibraryBundle> {
  const shellStore = useShellStore()
  const libraryId = shellStore.context?.activeLibrary.id
  if (!libraryId) {
    return {
      documents: [],
      mutations: [],
      jobs: [],
    }
  }

  const [documents, mutations, jobs] = await Promise.all([
    unwrap(
      apiHttp.get<RawContentDocumentDetailResponse[]>('/content/documents', {
        params: { libraryId, includeDeleted: false },
      }),
    ),
    unwrap(
      apiHttp.get<RawContentMutationDetailResponse[]>('/content/mutations', {
        params: { libraryId },
      }),
    ),
    unwrap(
      apiHttp.get<RawIngestJob[]>('/ingest/jobs', {
        params: {
          libraryId,
          workspaceId: shellStore.context?.activeWorkspace.id,
        },
      }),
    ),
  ])

  return {
    documents,
    mutations,
    jobs,
  }
}

function buildRelationMaps(
  bundle: CanonicalLibraryBundle,
): {
  mutationsByDocument: Map<string, RawContentMutationDetailResponse[]>
  jobsById: Map<string, RawIngestJob>
} {
  const mutationsByDocument = new Map<string, RawContentMutationDetailResponse[]>()
  for (const mutation of bundle.mutations) {
    for (const item of mutation.items) {
      if (!item.document_id) {
        continue
      }
      const current = mutationsByDocument.get(item.document_id) ?? []
      current.push(mutation)
      mutationsByDocument.set(item.document_id, current)
    }
  }
  for (const [documentId, mutations] of mutationsByDocument.entries()) {
    mutations.sort((left, right) => compareIsoDates(left.mutation.requested_at, right.mutation.requested_at))
    mutationsByDocument.set(documentId, mutations)
  }
  const jobsById = new Map(bundle.jobs.map((job) => [job.id, job] as const))
  return { mutationsByDocument, jobsById }
}

function buildDocumentRelations(
  documentId: string,
  bundle: CanonicalLibraryBundle,
  maps: ReturnType<typeof buildRelationMaps>,
): CanonicalDocumentRelation {
  const mutations = maps.mutationsByDocument.get(documentId) ?? []
  const jobIds = new Set<string>()
  const jobs: RawIngestJob[] = []

  for (const mutation of mutations) {
    if (!mutation.job_id || jobIds.has(mutation.job_id)) {
      continue
    }
    const job = maps.jobsById.get(mutation.job_id)
    if (!job) {
      continue
    }
    jobs.push(job)
    jobIds.add(job.id)
  }

  jobs.sort((left, right) => compareIsoDates(left.queued_at, right.queued_at))
  return { mutations, jobs }
}

function buildSurfaceResponse(
  bundle: CanonicalLibraryBundle,
  libraryName: string,
): DocumentsSurfaceResponse {
  const relationMaps = buildRelationMaps(bundle)
  const rows = bundle.documents
    .map((document) => {
      const relations = buildDocumentRelations(document.document.id, bundle, relationMaps)
      return mapSurfaceRow(document, relations.mutations, relations.jobs, libraryName)
    })
    .sort((left, right) => compareIsoDates(left.uploadedAt, right.uploadedAt))

  const diagnostics = buildCollectionDiagnostics(rows)

  return {
    acceptedFormats: DEFAULT_ACCEPTED_FORMATS,
    maxSizeMb: DEFAULT_UPLOAD_LIMIT_MB,
    graphStatus:
      rows.length === 0
        ? 'empty'
        : diagnostics.activeBacklogCount > 0
          ? 'building'
          : rows.some((row) => row.status === 'failed')
            ? 'failed'
            : rows.some((row) => row.status === 'ready_no_graph')
              ? 'partial'
              : 'ready',
    graphWarning:
      rows.length === 0
        ? null
        : diagnostics.activeBacklogCount > 0
          ? 'Canonical ingestion is still in flight.'
          : rows.some((row) => row.status === 'failed')
            ? 'One or more canonical documents failed ingestion.'
            : rows.some((row) => row.status === 'ready_no_graph')
              ? 'Some canonical documents are ready without graph coverage.'
              : null,
    rebuildBacklogCount: diagnostics.activeBacklogCount,
    counters: {
      queued: rows.filter((row) => row.status === 'queued').length,
      processing: rows.filter((row) => row.status === 'processing').length,
      ready: rows.filter((row) => row.status === 'ready').length,
      readyNoGraph: rows.filter((row) => row.status === 'ready_no_graph').length,
      failed: rows.filter((row) => row.status === 'failed').length,
    },
    filters: {
      statuses: Array.from(new Set(rows.map((row) => row.status))).sort(),
      fileTypes: Array.from(new Set(rows.map((row) => row.fileType))).sort(),
      accountingStatuses: Array.from(new Set(rows.map((row) => row.accountingStatus))).sort(),
      mutationStatuses: Array.from(
        new Set(
          rows
            .map((row) => row.mutation.status)
            .filter((value): value is NonNullable<DocumentRow['mutation']['status']> => value !== null),
        ),
      ).sort(),
    },
    accounting: buildCollectionAccounting(rows),
    diagnostics,
    workspace: buildWorkspaceSummary(rows, diagnostics),
    rows,
  }
}

export function normalizeDocumentUploadFailure(
  file: File,
  error: unknown,
): DocumentUploadFailure {
  const details =
    error instanceof ApiClientError ? normalizeUploadRejectionDetails(error.details) : null
  const message = error instanceof Error ? error.message : 'Failed to upload document'

  return {
    fileName: details?.fileName ?? file.name,
    message,
    errorKind: error instanceof ApiClientError ? error.errorKind : null,
    rejectionKind: details?.rejectionKind ?? null,
    detectedFormat: details?.detectedFormat ?? null,
    mimeType: details?.mimeType ?? (file.type || null),
    fileSizeBytes: details?.fileSizeBytes ?? file.size,
    uploadLimitMb: details?.uploadLimitMb ?? null,
    rejectionCause: details?.rejectionCause ?? null,
    operatorAction: details?.operatorAction ?? null,
  }
}

export async function fetchDocumentsSurface(): Promise<DocumentsSurfaceResponse> {
  const shellStore = useShellStore()
  const libraryName = shellStore.context?.activeLibrary.name ?? ''
  const bundle = await fetchCanonicalBundle()

  if (bundle.documents.length === 0) {
    return buildSurfaceResponse(bundle, libraryName)
  }

  return buildSurfaceResponse(bundle, libraryName)
}

export async function fetchDocumentDetail(id: string): Promise<DocumentDetail> {
  const shellStore = useShellStore()
  const document = await unwrap(apiHttp.get<RawContentDocumentDetailResponse>(`/content/documents/${id}`))
  const revisions = await unwrap(
    apiHttp.get<RawContentRevision[]>(`/content/documents/${id}/revisions`),
  )
  const mutations = await unwrap(
    apiHttp.get<RawContentMutationDetailResponse[]>('/content/mutations', {
      params: { libraryId: document.document.library_id },
    }),
  )
  const jobs = await unwrap(
    apiHttp.get<RawIngestJob[]>('/ingest/jobs', {
      params: {
        libraryId: document.document.library_id,
        workspaceId: document.document.workspace_id,
      },
    }),
  )
  const chunks = await unwrap(
    apiHttp.get<RawChunkSummary[]>('/chunks', {
      params: { document_id: id },
    }),
  )
  const relationMaps = buildRelationMaps({
    documents: [document],
    mutations,
    jobs,
  })
  const relations = buildDocumentRelations(id, { documents: [document], mutations, jobs }, relationMaps)
  const relatedMutations = relations.mutations
  const relatedJobs = relations.jobs
  const latestMutation = selectLatestMutation(document, relatedMutations)
  const revisionsById = new Map(revisions.map((revision) => [revision.id, revision] as const))
  const readableRevision = document.head?.readable_revision_id
    ? revisionsById.get(document.head.readable_revision_id) ?? null
    : document.active_revision
  const detail = mapDocumentDetail(
    document,
    revisions,
    chunks,
    relatedMutations,
    relatedJobs,
    latestMutation,
    shellStore.context?.activeLibrary.name ?? document.document.library_id,
  )

  if (readableRevision) {
    detail.canonical.readableRevision = buildCanonicalRevision(readableRevision)
  }

  return detail
}

function buildMutationRequestBase() {
  const shellStore = useShellStore()
  const workspace = shellStore.context?.activeWorkspace
  const library = shellStore.context?.activeLibrary
  if (!workspace || !library) {
    throw new Error('Active library is not selected')
  }
  return {
    workspace,
    library,
  }
}

async function readFileChecksum(file: File): Promise<string> {
  return sha256Hex(await file.arrayBuffer())
}

async function readTextChecksum(content: string): Promise<string> {
  return sha256Hex(content)
}

export async function uploadDocument(file: File): Promise<DocumentRow> {
  const { library } = buildMutationRequestBase()
  const formData = new FormData()
  formData.append('library_id', library.id)
  formData.append('file', file, file.name)
  formData.append('title', file.name)

  const response = await unwrap(
    apiHttp.post<RawCreateDocumentResponse>('/content/documents/upload', formData),
  )
  return fetchDocumentRowFromDetail(response.document.document.id)
}

export async function uploadDocuments(files: File[]): Promise<UploadDocumentsResponse> {
  const acceptedRows = await Promise.all(files.map((file) => uploadDocument(file)))
  return {
    acceptedRows,
    rejectedFiles: [],
  }
}

export async function deleteDocumentItem(id: string): Promise<void> {
  await unwrap(apiHttp.delete<RawContentMutationDetailResponse>(`/content/documents/${id}`))
}

export async function retryDocumentItem(id: string): Promise<DocumentRow> {
  const detail = await fetchDocumentDetail(id)
  const latestJob = detail.canonical.latestJob
  if (!latestJob) {
    throw new Error('No canonical ingest job is available for this document')
  }
  await unwrap(apiHttp.post<RawIngestJob>(`/ingest/jobs/${latestJob.id}/retry`))
  return await fetchDocumentRowFromDetail(id)
}

export async function reprocessDocumentItem(id: string): Promise<void> {
  const detail = await fetchDocumentDetail(id)
  const latestJob = detail.canonical.latestJob
  if (!latestJob) {
    throw new Error('No canonical ingest job is available for this document')
  }
  await unwrap(apiHttp.post<RawIngestJob>(`/ingest/jobs/${latestJob.id}/retry`))
}

async function fetchDocumentRowFromDetail(id: string): Promise<DocumentRow> {
  const detail = await fetchDocumentDetail(id)
  return {
    id: detail.id,
    logicalDocumentId: detail.logicalDocumentId,
    readabilityState: detail.readabilityState,
    activeRevisionId: detail.activeRevisionId,
    readableRevisionId: detail.readableRevisionId,
    readableRevisionNo: detail.readableRevisionNo,
    fileName: detail.fileName,
    fileType: detail.fileType,
    fileSizeLabel: detail.fileSizeLabel,
    uploadedAt: detail.uploadedAt,
    libraryName: detail.libraryName,
    stage: detail.stage,
    status: detail.status,
    progressPercent: detail.progressPercent,
    activityStatus: detail.activityStatus,
    lastActivityAt: detail.lastActivityAt,
    stalledReason: detail.stalledReason,
    chunkCount: detail.extractedStats.chunkCount,
    graphNodeCount: detail.graphStats.nodeCount,
    graphEdgeCount: detail.graphStats.edgeCount,
    activeRevisionNo: detail.activeRevisionNo,
    activeRevisionKind: detail.activeRevisionKind,
    latestAttemptNo: detail.latestAttemptNo,
    accountingStatus: detail.accountingStatus,
    totalEstimatedCost: detail.totalEstimatedCost,
    settledEstimatedCost: detail.settledEstimatedCost,
    inFlightEstimatedCost: detail.inFlightEstimatedCost,
    currency: detail.currency,
    inFlightStageCount: detail.inFlightStageCount,
    missingStageCount: detail.missingStageCount,
    partialHistory: detail.partialHistory,
    partialHistoryReason: detail.partialHistoryReason,
    graphThroughput: detail.graphThroughput,
    mutation: detail.mutation,
    canRetry: detail.canRetry,
    canAppend: detail.canAppend,
    canReplace: detail.canReplace,
    canRemove: detail.canRemove,
    detailAvailable: detail.detailAvailable,
    canonical: detail.canonical,
  }
}

async function buildMutationAcceptedResponse(
  response: RawContentMutationDetailResponse,
): Promise<DocumentMutationAccepted> {
  return {
    accepted: true,
    operation: response.mutation.operation_kind,
    trackId: response.job_id,
    revisionId:
      response.items.find((item) => item.result_revision_id !== null)?.result_revision_id ?? null,
    mutationId: response.mutation.id,
    attemptNo: response.job_id ? 1 : null,
  }
}

export async function appendDocumentItem(
  libraryId: string,
  id: string,
  content: string,
): Promise<DocumentMutationAccepted> {
  const checksum = await readTextChecksum(content)
  const response = await unwrap(
    apiHttp.post<RawContentMutationDetailResponse>(`/content/documents/${id}/append`, {
      appendedText: content,
      idempotencyKey: `append:${libraryId}:${id}:${checksum}`,
    }),
  )
  return buildMutationAcceptedResponse(response)
}

export async function replaceDocumentItem(
  libraryId: string,
  id: string,
  file: File,
): Promise<DocumentMutationAccepted> {
  const checksum = await readFileChecksum(file)
  const formData = new FormData()
  formData.append('file', file, file.name)
  formData.append('idempotency_key', `replace:${libraryId}:${id}:${checksum}`)
  const response = await unwrap(
    apiHttp.post<RawContentMutationDetailResponse>(
      `/content/documents/${id}/replace`,
      formData,
    ),
  )
  return buildMutationAcceptedResponse(response)
}

export async function downloadDocumentExtractedText(id: string): Promise<Blob> {
  const chunks = await unwrap(
    apiHttp.get<RawChunkSummary[]>('/chunks', {
      params: { document_id: id },
    }),
  )
  if (chunks.length === 0) {
    throw new Error('No extracted text is available for this document')
  }
  const content = chunks
    .slice()
    .sort((left, right) => left.ordinal - right.ordinal)
    .map((chunk) => chunk.content)
    .join('\n\n')
  return new Blob([content], { type: 'text/plain;charset=utf-8' })
}
