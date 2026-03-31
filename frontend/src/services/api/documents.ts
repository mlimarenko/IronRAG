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
  DocumentRowSummary,
  DocumentStatus,
  DocumentSummaryCounters,
  DocumentsWorkspaceDiagnosticChip,
  DocumentsWorkspaceNotice,
  DocumentsWorkspacePrimarySummary,
  DocumentsWorkspaceSummary,
  DocumentsSurfaceResponse,
  DocumentUploadFailure,
  DocumentCostEntry,
  LibraryCostSummary,
  UploadDocumentsResponse,
  UploadRejectionDetails,
} from 'src/models/ui/documents'
import type { DashboardRecentDocument } from 'src/models/ui/dashboard'
import {
  DOCUMENT_UPLOAD_FORMAT_TOKENS,
  inferDocumentFileType,
} from 'src/models/ui/documentFormats'
import type { GraphCanonicalSummary } from 'src/models/ui/graph'
import { i18n } from 'src/lib/i18n'
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

interface RawContentRevisionReadiness {
  revision_id: string
  text_state: string
  vector_state: string
  graph_state: string
  text_readable_at: string | null
  vector_ready_at: string | null
  graph_ready_at: string | null
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
  file_name: string
  head: RawContentDocumentHead | null
  active_revision: RawContentRevision | null
  readiness: RawContentRevisionReadiness | null
  pipeline: RawContentDocumentPipelineState
}

interface RawContentDocumentPipelineJob {
  id: string
  workspace_id: string
  library_id: string
  mutation_id: string | null
  async_operation_id: string | null
  job_kind: string
  queue_state: 'queued' | 'leased' | 'completed' | 'failed' | 'canceled' | string
  queued_at: string
  available_at: string
  completed_at: string | null
  current_stage: string | null
  failure_code: string | null
  retryable: boolean
}

interface RawContentDocumentPipelineState {
  latest_mutation: RawContentMutation | null
  latest_job: RawContentDocumentPipelineJob | null
}

interface RawKnowledgeRevisionRow {
  key: string
  arangoId?: string | null
  arangoRev?: string | null
  revisionId: string
  workspaceId: string
  libraryId: string
  documentId: string
  revisionNumber: number
  revisionState: string
  revisionKind: string
  storageRef: string | null
  mimeType: string
  checksum: string
  title: string | null
  byteSize: number
  normalizedText: string | null
  textChecksum: string | null
  textState: string
  vectorState: string
  graphState: string
  textReadableAt: string | null
  vectorReadyAt: string | null
  graphReadyAt: string | null
  supersededByRevisionId: string | null
  createdAt: string
}

interface RawKnowledgeDocumentDetailResponse {
  document: RawContentDocument
  revisions: RawKnowledgeRevisionRow[]
  latestRevision: RawKnowledgeRevisionRow | null
  latestRevisionChunks: RawChunkSummary[]
}

interface RawContentMutationDetailResponse {
  mutation: RawContentMutation
  items: RawContentMutationItem[]
  job_id: string | null
  async_operation_id?: string | null
}

interface RawContentMutationDetailResponseWire {
  mutation: RawContentMutation
  items: RawContentMutationItem[]
  job_id?: string | null
  jobId?: string | null
  async_operation_id?: string | null
  asyncOperationId?: string | null
}

interface RawCreateDocumentResponse {
  document: RawContentDocumentDetailResponse
  mutation: RawContentMutationDetailResponseWire
}

interface RawIngestJob {
  id: string
  workspace_id: string
  library_id: string
  mutation_id: string | null
  connector_id: string | null
  async_operation_id?: string | null
  job_kind: string
  queue_state: 'queued' | 'leased' | 'completed' | 'failed' | 'canceled' | string
  priority: number
  dedupe_key: string | null
  queued_at: string
  available_at: string
  completed_at: string | null
  current_stage?: string | null
  failure_code?: string | null
  retryable?: boolean
}

interface RawIngestAttempt {
  id: string
  job_id: string
  attempt_number: number
  attempt_state: string
  current_stage: string | null
  started_at: string
  heartbeat_at: string | null
  finished_at: string | null
  failure_class: string | null
  failure_code: string | null
  retryable: boolean
}

interface RawIngestJobResponse {
  job: RawIngestJob
  latest_attempt?: RawIngestAttempt | null
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

const DEFAULT_ACCEPTED_FORMATS = [...DOCUMENT_UPLOAD_FORMAT_TOKENS]
const DEFAULT_UPLOAD_LIMIT_MB = 50
const STALLED_ACTIVITY_AFTER_MS = 180_000

type RawWireRecord = Record<string, unknown>

function readString(record: Record<string, unknown>, key: string): string | null {
  const value = record[key]
  return typeof value === 'string' ? value : null
}

function readNumber(record: Record<string, unknown>, key: string): number | null {
  const value = record[key]
  return typeof value === 'number' ? value : null
}

function normalizeContentDocumentDetailResponse(
  payload: RawContentDocumentDetailResponse | RawWireRecord,
): RawContentDocumentDetailResponse {
  const detail = payload as RawContentDocumentDetailResponse & {
    fileName?: string | null
    activeRevision?: RawContentRevision | null
    readiness?: RawContentRevisionReadiness | null
    pipeline?: RawContentDocumentPipelineState | null
  }
  return {
    document: detail.document,
    file_name: detail.file_name ?? detail.fileName ?? detail.document.external_key,
    head: detail.head ?? null,
    active_revision: detail.active_revision ?? detail.activeRevision ?? null,
    readiness: detail.readiness ?? null,
    pipeline: detail.pipeline ?? {
      latest_mutation: null,
      latest_job: null,
    },
  }
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

const STAGE_LABELS: Record<string, string> = {
  extract_content: 'extracting_content',
  chunk_content: 'chunking',
  embed_chunk: 'embedding_chunks',
  extract_graph: 'building_graph',
  finalizing: 'finalizing',
}

function documentStageLabel(stage: string): string {
  const key = `documents.stage.${stage}`
  return i18n.global.te(key) ? i18n.global.t(key) : stage
}

function stageKeyFor(
  job: RawIngestJob | null,
  mutationState: string | null = null,
): string | null {
  if (job) {
    const state = job.queue_state
    if (state === 'queued') return 'accepted'
    if (state === 'leased' && job.current_stage) {
      const canonicalStage = STAGE_LABELS[job.current_stage] ?? job.current_stage
      return canonicalStage === 'content_mutation' ? 'claimed' : canonicalStage
    }
    if (state === 'leased') return 'claimed'
    if (state === 'failed' || state === 'canceled') return 'failed'
    if (state === 'completed') return 'completed'
  }

  switch (mutationState) {
    case 'accepted':
      return 'accepted'
    case 'running':
      return 'claimed'
    case 'failed':
    case 'conflicted':
    case 'canceled':
      return 'failed'
    case 'applied':
      return 'completed'
    default:
      return null
  }
}

function stageLabelFor(job: RawIngestJob | null, mutationState: string | null = null): string | null {
  const stageKey = stageKeyFor(job, mutationState)
  if (!stageKey) return null
  if (stageKey === 'completed') return null
  return documentStageLabel(stageKey)
}

function progressPercentForState(
  status: DocumentStatus,
  activityStatus: DocumentActivityStatus,
  stageKey: string | null,
): number | null {
  if (status === 'failed') {
    return null
  }

  if (status === 'ready') {
    return 100
  }

  if (status === 'ready_no_graph') {
    return 92
  }

  switch (stageKey) {
    case 'accepted':
      return 8
    case 'claimed':
      return 16
    case 'extracting_content':
      return 34
    case 'chunking':
      return 58
    case 'embedding_chunks':
      return 74
    case 'building_graph':
      return 88
    case 'finalizing':
      return 96
    case 'completed':
      return 100
    default:
      return activityStatus === 'queued' ? 8 : activityStatus === 'active' ? 24 : null
  }
}

function terminalStageForStatus(status: DocumentStatus): string {
  return status === 'failed' ? 'failed' : 'completed'
}

function currentMutationState(
  mutation: RawContentMutationDetailResponse | null,
  fallbackMutation: RawContentMutationDetailResponse | null = null,
): string | null {
  return mutation?.mutation.mutation_state ?? fallbackMutation?.mutation.mutation_state ?? null
}

function stageKeyForDetail(
  status: DocumentStatus,
  job: RawIngestJob | null,
  mutationState: string | null,
): string {
  return stageKeyFor(job, mutationState) ?? terminalStageForStatus(status)
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

function isReadableTextState(textState: string | null | undefined): boolean {
  return textState === 'readable' || textState === 'ready' || textState === 'text_readable'
}

function readinessTextState(readiness: RawContentRevisionReadiness | RawKnowledgeRevisionRow): string {
  return 'text_state' in readiness ? readiness.text_state : readiness.textState
}

function readinessVectorState(readiness: RawContentRevisionReadiness | RawKnowledgeRevisionRow): string {
  return 'vector_state' in readiness ? readiness.vector_state : readiness.vectorState
}

function readinessGraphState(readiness: RawContentRevisionReadiness | RawKnowledgeRevisionRow): string {
  return 'graph_state' in readiness ? readiness.graph_state : readiness.graphState
}

function isFailedReadiness(readiness: RawContentRevisionReadiness | RawKnowledgeRevisionRow | null): boolean {
  if (!readiness) {
    return false
  }
  return (
    readinessTextState(readiness) === 'failed' ||
    readinessTextState(readiness) === 'unavailable' ||
    readinessVectorState(readiness) === 'failed' ||
    readinessGraphState(readiness) === 'failed'
  )
}

function isGraphReadyState(graphState: string | null | undefined): boolean {
  return graphState === 'ready' || graphState === 'graph_ready'
}

function isOpenMutationState(mutationState: string | null | undefined): boolean {
  return mutationState === 'accepted' || mutationState === 'running'
}

function isFailedMutationState(mutationState: string | null | undefined): boolean {
  return mutationState === 'failed' || mutationState === 'conflicted' || mutationState === 'canceled'
}

function documentStatusFromCurrentState(
  readiness: RawContentRevisionReadiness | RawKnowledgeRevisionRow | null,
  mutationState: string | null,
  queueState: RawIngestJob['queue_state'] | null,
): DocumentStatus {
  if (isOpenMutationState(mutationState)) {
    return queueState === 'queued' ? 'queued' : 'processing'
  }

  if (isFailedMutationState(mutationState)) {
    return 'failed'
  }

  if (readiness) {
    if (isFailedReadiness(readiness)) {
      return 'failed'
    }
    if (isReadableTextState(readinessTextState(readiness))) {
      return isGraphReadyState(readinessGraphState(readiness)) ? 'ready' : 'ready_no_graph'
    }
  }

  return documentStatusFromQueueState(queueState, false)
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

function activityStatusFromCurrentState(
  status: DocumentStatus,
  currentJob: RawIngestJob | null,
  mutationState: string | null,
): DocumentActivityStatus {
  if (currentJob) {
    return activityStatusFromQueueState(currentJob.queue_state, latestActivityAt(currentJob))
  }

  if (isOpenMutationState(mutationState)) {
    return 'active'
  }

  if (status === 'failed') {
    return 'failed'
  }

  if (status === 'ready' || status === 'ready_no_graph') {
    return 'ready'
  }

  return 'queued'
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
    currentStage: stageKeyFor(job),
    startedAt: job.queued_at,
    heartbeatAt: lastActivity,
    finishedAt: job.completed_at,
    failureClass: job.queue_state === 'failed' || job.queue_state === 'canceled' ? 'ingest_failed' : null,
    failureCode: null,
    retryable: Boolean(job.retryable),
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
  detail: RawContentDocumentDetailResponse,
  activeRevision: RawContentRevision | null,
  revisions: RawContentRevision[],
): string {
  return detail.file_name ?? activeRevision?.title ?? revisions[0]?.title ?? detail.document.external_key
}

function buildDocumentFileType(
  detail: RawContentDocumentDetailResponse,
  activeRevision: RawContentRevision | null,
  revisions: RawContentRevision[],
): string {
  const reference = activeRevision ?? revisions[0] ?? null
  if (!reference) {
    return inferDocumentFileType(detail.file_name, null)
  }
  const fileNameReference =
    reference.source_uri?.replace(/^upload:\/\//, '') ??
    detail.file_name
  return inferDocumentFileType(fileNameReference, reference.mime_type)
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
    stage: stageKeyFor(job) ?? 'accepted',
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

function buildKnowledgeReadiness(
  revision: RawKnowledgeRevisionRow | RawContentRevisionReadiness | null,
): DocumentDetail['knowledgeReadiness'] {
  if (!revision) {
    return null
  }

  const revisionId = 'revisionId' in revision ? revision.revisionId : revision.revision_id
  const revisionNo = 'revisionNumber' in revision ? revision.revisionNumber : null
  const revisionKind = 'revisionKind' in revision ? revision.revisionKind : null
  const textState = 'textState' in revision ? revision.textState : revision.text_state
  const vectorState = 'vectorState' in revision ? revision.vectorState : revision.vector_state
  const graphState = 'graphState' in revision ? revision.graphState : revision.graph_state
  const textReadableAt =
    'textReadableAt' in revision ? revision.textReadableAt : revision.text_readable_at
  const vectorReadyAt =
    'vectorReadyAt' in revision ? revision.vectorReadyAt : revision.vector_ready_at
  const graphReadyAt =
    'graphReadyAt' in revision ? revision.graphReadyAt : revision.graph_ready_at

  return {
    revisionId,
    revisionNo,
    revisionKind,
    textState,
    vectorState,
    graphState,
    textReadableAt,
    vectorReadyAt,
    graphReadyAt,
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
  rows: DocumentRowSummary[],
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

function buildCollectionDiagnostics(rows: DocumentRowSummary[]): DocumentCollectionDiagnostics {
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
  const writeHealth: DocumentGraphHealthSummary['writeHealth'] =
    failed > 0 ? 'failed' : activeBacklogCount > 0 ? 'retrying_contention' : 'healthy'
  const graphHealth: DocumentGraphHealthSummary = {
    writeHealth,
    activeWriteCount: ready,
    retryingWriteCount: processing,
    failedWriteCount: failed,
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
            .map((row) => row.uploadedAt)
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
  rows: DocumentRowSummary[],
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
        ? i18n.global.t('documents.workspace.primaryValues.inFlight')
        : failed > 0
          ? i18n.global.t('documents.workspace.primaryValues.residualWork')
          : i18n.global.t('documents.workspace.primaryValues.settled'),
    backlogLabel:
      backlogCount > 0
        ? i18n.global.t('documents.workspace.primaryValues.pending', { count: backlogCount })
        : i18n.global.t('documents.workspace.primaryValues.clear'),
    terminalState: diagnostics.terminalOutcome?.terminalState ?? 'live_in_flight',
  }
  const secondaryDiagnostics: DocumentsWorkspaceDiagnosticChip[] = [
    {
      kind: 'documents',
      label: i18n.global.t('shell.documents'),
      value: String(rows.length),
    },
    {
      kind: 'queue',
      label: i18n.global.t('documents.queued'),
      value: String(queued),
    },
    {
      kind: 'processing',
      label: i18n.global.t('documents.processing'),
      value: String(processing),
    },
    {
      kind: 'failed',
      label: i18n.global.t('documents.failed'),
      value: String(failed),
    },
  ]
  const degradedNotices: DocumentsWorkspaceNotice[] = []
  const informationalNotices: DocumentsWorkspaceNotice[] = []

  if (backlogCount > 0) {
    informationalNotices.push({
      kind: 'ordinary_backlog',
      title: i18n.global.t('documents.workspace.notices.pipelineActive.title'),
      message: i18n.global.t('documents.workspace.notices.pipelineActive.message', {
        count: backlogCount,
      }),
    })
  }

  if (failed > 0) {
    degradedNotices.push({
      kind: 'failed_work',
      title: i18n.global.t('documents.workspace.notices.pipelineFailed.title'),
      message: i18n.global.t('documents.workspace.notices.pipelineFailed.message', {
        count: failed,
      }),
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

function selectMutationJob(
  document: RawContentDocumentDetailResponse,
  mutation: RawContentMutationDetailResponse | null,
  relatedJobs: RawIngestJob[],
): RawIngestJob | null {
  if (document.pipeline.latest_job) {
    return mapPipelineJobToRawIngestJob(document.pipeline.latest_job)
  }
  if (mutation?.job_id) {
    const job = relatedJobs.find((item) => item.id === mutation.job_id)
    if (job) {
      return job
    }
  }
  return null
}

function selectCurrentMutation(
  document: RawContentDocumentDetailResponse,
  relatedMutations: RawContentMutationDetailResponse[],
): RawContentMutationDetailResponse | null {
  if (document.pipeline.latest_mutation) {
    return {
      mutation: document.pipeline.latest_mutation,
      items: [],
      job_id: document.pipeline.latest_job?.id ?? null,
      async_operation_id: document.pipeline.latest_job?.async_operation_id ?? null,
    }
  }
  if (!document.head?.latest_mutation_id) {
    return null
  }
  return (
    relatedMutations.find((mutation) => mutation.mutation.id === document.head?.latest_mutation_id) ??
    null
  )
}

function mapPipelineJobToRawIngestJob(job: RawContentDocumentPipelineJob): RawIngestJob {
  return {
    id: job.id,
    workspace_id: job.workspace_id,
    library_id: job.library_id,
    mutation_id: job.mutation_id,
    connector_id: null,
    async_operation_id: job.async_operation_id,
    job_kind: job.job_kind,
    queue_state: job.queue_state,
    priority: 0,
    dedupe_key: null,
    queued_at: job.queued_at,
    available_at: job.available_at,
    completed_at: job.completed_at,
    current_stage: job.current_stage,
    failure_code: job.failure_code,
    retryable: job.retryable,
  }
}

function mapSurfaceRow(
  document: RawContentDocumentDetailResponse,
  relatedMutations: RawContentMutationDetailResponse[],
  relatedJobs: RawIngestJob[],
): DocumentRowSummary {
  const revisions = document.active_revision ? [document.active_revision] : []
  const currentMutation = selectCurrentMutation(document, relatedMutations)
  const currentJob = selectMutationJob(document, currentMutation, relatedJobs)
  const currentMutationStatus = currentMutationState(currentMutation)
  const documentStatus = documentStatusFromCurrentState(
    document.readiness,
    currentMutationStatus,
    currentJob?.queue_state ?? null,
  )
  const activityStatus = activityStatusFromCurrentState(documentStatus, currentJob, currentMutationStatus)
  const stage = stageKeyFor(currentJob, currentMutationStatus)
  const activeRevision = document.active_revision
  const fileName = buildDocumentFileName(document, activeRevision, revisions)
  const fileType = buildDocumentFileType(document, activeRevision, revisions)
  const lastActivityAt = latestActivityAt(currentJob)

  return {
    id: document.document.id,
    fileName,
    fileType,
    fileSizeBytes: activeRevision?.byte_size ?? null,
    fileSizeLabel: activeRevision ? formatFileSizeLabel(activeRevision.byte_size) : '—',
    uploadedAt: document.document.created_at,
    status: documentStatus,
    statusLabel: statusLabelFor(documentStatus),
    stage,
    stageLabel: stageLabelFor(currentJob, currentMutationStatus),
    progressPercent: progressPercentForState(documentStatus, activityStatus, stage),
    activityStatus,
    lastActivityAt,
    stalledReason: stalledReason(activityStatus, lastActivityAt),
    costAmount: null,
    costLabel: null,
    canRetry:
      documentStatus === 'failed' &&
      document.document.document_state === 'active' &&
      activeRevision !== null &&
      currentJob?.retryable === true,
    detailAvailable: document.document.document_state === 'active',
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
  knowledgeRevision: RawKnowledgeRevisionRow | null,
): DocumentDetail {
  const activeRevision = document.active_revision
  const readableRevision =
    document.head?.readable_revision_id
      ? revisions.find((revision) => revision.id === document.head?.readable_revision_id) ?? null
      : activeRevision
  const currentMutation = selectCurrentMutation(document, relatedMutations)
  const currentJob = selectMutationJob(document, currentMutation, relatedJobs)
  const readiness = knowledgeRevision ?? document.readiness
  const status = documentStatusFromCurrentState(
    readiness,
    currentMutation?.mutation.mutation_state ?? null,
    currentJob?.queue_state ?? null,
  )
  const activityStatus = activityStatusFromCurrentState(
    status,
    currentJob,
    currentMutation?.mutation.mutation_state ?? null,
  )
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
  const knowledgeReadiness = buildKnowledgeReadiness(readiness)
  const mutationState = buildMutationState((currentMutation ?? latestMutation)?.mutation ?? null)
  const lastActivity = latestActivityAt(currentJob)
  const attemptGroups = mapDetailAttempts(relatedJobs, activeRevision)
  const processingHistory = mapDetailProcessingHistory(relatedJobs)
  const latestJobForCanonical = currentJob ?? null
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
    fileName: buildDocumentFileName(document, activeRevision, revisions),
    fileType: buildDocumentFileType(document, activeRevision, revisions),
    fileSizeLabel: activeRevision ? formatFileSizeLabel(activeRevision.byte_size) : '—',
    uploadedAt: document.document.created_at,
    libraryName,
    stage: stageKeyForDetail(
      status,
      currentJob,
      currentMutationState(currentMutation, latestMutation),
    ),
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
    providerCallCount: 0,
    inFlightStageCount: activityStatus === 'queued' || activityStatus === 'active' ? 1 : 0,
    missingStageCount: 0,
    partialHistory: relatedJobs.length > 1 || status !== 'ready',
    partialHistoryReason:
      (currentMutation ?? latestMutation)?.mutation.failure_code ??
      (currentMutation ?? latestMutation)?.mutation.conflict_code ??
      null,
    mutation: mutationState,
    requestedBy: (currentMutation ?? latestMutation)?.mutation.requested_by_principal_id ?? null,
    errorMessage:
      currentMutation?.mutation.failure_code ??
      currentMutation?.mutation.conflict_code ??
      (status === 'failed' ? 'Canonical ingestion failed' : null),
    failureClass:
      currentJob?.queue_state === 'failed' || currentJob?.queue_state === 'canceled'
        ? 'ingest_failed'
        : null,
    operatorAction: (currentMutation ?? latestMutation)?.mutation.operation_kind ?? null,
    summary:
      canonicalSummaryPreview?.text ??
      activeRevision?.title ??
      document.document.external_key,
    graphNodeId:
      activeRevision && knowledgeReadiness?.graphState === 'ready' ? document.document.id : null,
    canonicalSummaryPreview,
    canDownloadText: previewChunkCount > 0,
    canAppend: document.document.document_state === 'active',
    canReplace: document.document.document_state === 'active',
    canRemove: document.document.document_state === 'active',
    canRetry:
      status === 'failed' &&
      document.document.document_state === 'active' &&
      activeRevision !== null &&
      currentJob?.retryable === true,
    detailAvailable: document.document.document_state === 'active',
    reconciliationScope: null,
    providerFailure: null,
    graphThroughput: buildGraphThroughput(
      activityStatus === 'queued' || activityStatus === 'active' ? 1 : 0,
      Math.max(1, previewChunkCount),
      previewChunkCount,
      activityStatus === 'queued' || activityStatus === 'active' ? 'elevated' : null,
    ),
    knowledgeReadiness,
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
      currentMutation ?? latestMutation,
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

async function fetchSurfaceDocumentsForLibrary(
  libraryId?: string | null,
): Promise<RawContentDocumentDetailResponse[]> {
  const resolvedLibraryId = libraryId ?? useShellStore().context?.activeLibrary.id ?? null
  if (!resolvedLibraryId) {
    return []
  }

  const documents = await unwrap(
    apiHttp.get<RawContentDocumentDetailResponse[]>('/content/documents', {
      params: { libraryId: resolvedLibraryId, includeDeleted: false },
    }),
  )

  return documents.map(normalizeContentDocumentDetailResponse)
}

function normalizeIngestJobResponse(response: RawIngestJobResponse | RawIngestJob): RawIngestJob {
  if ('job' in response) {
    const job = { ...response.job }
    const attempt = response.latest_attempt
    if (attempt) {
      job.current_stage = attempt.current_stage
      job.failure_code = attempt.failure_code
      job.retryable = attempt.retryable
    } else if (job.retryable === undefined) {
      job.retryable = false
    }
    return job
  }
  return {
    ...response,
    retryable: response.retryable ?? false,
  }
}

function normalizeContentMutationDetailResponse(
  response: RawContentMutationDetailResponseWire | RawContentMutationDetailResponse,
): RawContentMutationDetailResponse {
  const jobId = 'jobId' in response ? response.jobId : null
  const asyncOperationId = 'asyncOperationId' in response ? response.asyncOperationId : null
  return {
    mutation: response.mutation,
    items: response.items,
    job_id: response.job_id ?? jobId ?? null,
    async_operation_id: response.async_operation_id ?? asyncOperationId ?? null,
  }
}

async function fetchKnowledgeDocumentDetail(
  libraryId: string,
  documentId: string,
): Promise<RawKnowledgeDocumentDetailResponse> {
  return unwrap(
    apiHttp.get<RawKnowledgeDocumentDetailResponse>(
      `/knowledge/libraries/${libraryId}/documents/${documentId}`,
    ),
  )
}

function buildSurfaceResponse(
  documents: RawContentDocumentDetailResponse[],
): DocumentsSurfaceResponse {
  const rows = documents
    .map((document) => mapSurfaceRow(document, [], []))
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
    },
    accounting: buildCollectionAccounting(rows),
    diagnostics,
    workspace: buildWorkspaceSummary(rows, diagnostics),
    rows,
  }
}

export function mapDashboardRecentDocuments(
  rows: DocumentRowSummary[],
  limit = 8,
): DashboardRecentDocument[] {
  return rows
    .slice()
    .sort((left, right) => {
      const uploadedAtDelta = compareIsoDates(left.uploadedAt, right.uploadedAt)
      if (uploadedAtDelta !== 0) {
        return uploadedAtDelta
      }
      return left.fileName.localeCompare(right.fileName)
    })
    .slice(0, limit)
    .map((row) => ({
      id: row.id,
      fileName: row.fileName,
      fileType: row.fileType,
      fileSizeLabel: row.fileSizeLabel,
      status: row.status,
      statusLabel: row.statusLabel,
      uploadedAt: row.uploadedAt,
    }))
}

export function normalizeDocumentUploadFailure(
  file: File,
  error: unknown,
): DocumentUploadFailure {
  const details =
    error instanceof ApiClientError ? normalizeUploadRejectionDetails(error.details) : null
  const rawMessage = error instanceof Error ? error.message : 'Failed to upload document'
  const message = summarizeUploadFailureMessage(rawMessage, details)

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

function summarizeUploadFailureMessage(
  rawMessage: string,
  details: UploadRejectionDetails | null,
): string {
  const rejectionCause = details?.rejectionCause?.trim()
  if (rejectionCause) {
    return rejectionCause
  }

  const sanitized = rawMessage.split(' body=')[0]?.trim() ?? rawMessage.trim()
  const providerStatusMatch = sanitized.match(/provider=([a-z0-9_-]+)\s+status=([0-9]{3})\s+(.+)$/i)
  if (providerStatusMatch) {
    const [, provider, statusCode, statusText] = providerStatusMatch
    const providerLabel = provider.charAt(0).toUpperCase() + provider.slice(1)
    return `${providerLabel} request failed (${statusCode} ${statusText.trim()})`
  }

  return sanitized
    .replace(/^graph provider call failed(?: for chunk [^:]+)?:\s*/i, 'Graph extraction failed: ')
    .replace(/^provider request failed:\s*/i, '')
    .replace(/\s+/g, ' ')
    .trim()
}

interface RawDocumentCostEntry {
  documentId: string
  totalCost: string
  currencyCode: string
  providerCallCount: number
}

interface RawLibraryCostSummary {
  totalCost: string
  currencyCode: string
  documentCount: number
  providerCallCount: number
}

function formatCostLabel(costUsd: number): string {
  if (costUsd <= 0) return ''
  if (costUsd < 0.001) return '<$0.001'
  if (costUsd < 0.01) return `$${costUsd.toFixed(4).replace(/\.?0+$/, '')}`
  return `$${costUsd.toFixed(3).replace(/\.?0+$/, '')}`
}

export async function fetchLibraryDocumentCosts(libraryId: string): Promise<DocumentCostEntry[]> {
  try {
    const raw = await unwrap(
      apiHttp.get<RawDocumentCostEntry[]>('/billing/library-document-costs', {
        params: { libraryId },
      }),
    )
    return raw.map((entry) => ({
      documentId: entry.documentId,
      totalCost: Number(entry.totalCost),
      currencyCode: entry.currencyCode,
      providerCallCount: entry.providerCallCount,
    }))
  } catch {
    return []
  }
}

export async function fetchLibraryCostSummary(libraryId: string): Promise<LibraryCostSummary | null> {
  try {
    const raw = await unwrap(
      apiHttp.get<RawLibraryCostSummary>('/billing/library-cost-summary', {
        params: { libraryId },
      }),
    )
    return {
      totalCost: Number(raw.totalCost),
      currencyCode: raw.currencyCode,
      documentCount: raw.documentCount,
      providerCallCount: raw.providerCallCount,
    }
  } catch {
    return null
  }
}

export async function fetchDocumentsSurface(): Promise<DocumentsSurfaceResponse> {
  const shellStore = useShellStore()
  const libraryId = shellStore.context?.activeLibrary.id ?? null
  const [documents, documentCosts] = await Promise.all([
    fetchSurfaceDocumentsForLibrary(libraryId),
    libraryId ? fetchLibraryDocumentCosts(libraryId) : Promise.resolve([]),
  ])

  const surface = buildSurfaceResponse(documents)

  if (documentCosts.length > 0) {
    const costMap = new Map(documentCosts.map((c) => [c.documentId, c]))
    for (const row of surface.rows) {
      const cost = costMap.get(row.id)
      if (cost && cost.totalCost > 0) {
        row.costAmount = cost.totalCost
        row.costLabel = formatCostLabel(cost.totalCost)
      }
    }
  }

  return surface
}

export async function fetchDocumentSummaryCounters(
  libraryId?: string | null,
): Promise<DocumentSummaryCounters> {
  const documents = await fetchSurfaceDocumentsForLibrary(libraryId)
  return buildSurfaceResponse(documents).counters
}

export async function fetchDocumentDetail(id: string): Promise<DocumentDetail> {
  const shellStore = useShellStore()
  const document = normalizeContentDocumentDetailResponse(
    await unwrap(apiHttp.get<RawContentDocumentDetailResponse>(`/content/documents/${id}`)),
  )
  const [revisions, chunks, knowledgeDetail, documentCosts] = await Promise.all([
    unwrap(apiHttp.get<RawContentRevision[]>(`/content/documents/${id}/revisions`)),
    unwrap(
      apiHttp.get<RawChunkSummary[]>('/chunks', {
        params: { documentId: id },
      }),
    ),
    fetchKnowledgeDocumentDetail(document.document.library_id, id),
    fetchLibraryDocumentCosts(document.document.library_id),
  ])
  const relatedMutations: RawContentMutationDetailResponse[] = []
  const relatedJobs: RawIngestJob[] = []
  const effectiveKnowledgeRevision = document.readiness?.revision_id
    ? knowledgeDetail.revisions.find((revision) => revision.revisionId === document.readiness?.revision_id) ??
      knowledgeDetail.latestRevision
    : knowledgeDetail.latestRevision
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
    null,
    shellStore.context?.activeLibrary.name ?? document.document.library_id,
    effectiveKnowledgeRevision,
  )

  if (readableRevision) {
    detail.canonical.readableRevision = buildCanonicalRevision(readableRevision)
  }

  const documentCost = documentCosts.find((entry) => entry.documentId === id) ?? null
  if (documentCost && documentCost.totalCost > 0) {
    detail.totalEstimatedCost = documentCost.totalCost
    detail.settledEstimatedCost = documentCost.totalCost
    detail.currency = documentCost.currencyCode
    detail.providerCallCount = documentCost.providerCallCount
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

export async function uploadDocument(file: File): Promise<DocumentRowSummary> {
  const { library } = buildMutationRequestBase()
  const formData = new FormData()
  formData.append('library_id', library.id)
  formData.append('file', file, file.name)
  formData.append('title', file.name)

  const response = await unwrap(
    apiHttp.post<RawCreateDocumentResponse>('/content/documents/upload', formData),
  )
  const document = normalizeContentDocumentDetailResponse(response.document)
  const mutation = normalizeContentMutationDetailResponse(response.mutation)
  return mapSurfaceRow(document, [mutation], [])
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

export async function retryDocumentItem(id: string): Promise<DocumentRowSummary> {
  await unwrap(
    apiHttp.post<RawContentMutationDetailResponse>(
      `/content/documents/${id}/reprocess`,
      {},
    ),
  )
  return await fetchDocumentRowFromDetail(id)
}

async function fetchDocumentRowFromDetail(id: string): Promise<DocumentRowSummary> {
  const detail = await fetchDocumentDetail(id)
  return {
    id: detail.id,
    fileName: detail.fileName,
    fileType: detail.fileType,
    fileSizeBytes: detail.canonical.activeRevision?.byteSize ?? null,
    fileSizeLabel: detail.fileSizeLabel,
    uploadedAt: detail.uploadedAt,
    status: detail.status,
    statusLabel: statusLabelFor(detail.status),
    stage: detail.stage,
    stageLabel:
      detail.status === 'processing' || detail.status === 'queued'
        ? documentStageLabel(detail.stage)
        : null,
    progressPercent: detail.progressPercent,
    activityStatus: detail.activityStatus,
    lastActivityAt: detail.lastActivityAt,
    stalledReason: detail.stalledReason,
    costAmount: detail.totalEstimatedCost && detail.totalEstimatedCost > 0
      ? detail.totalEstimatedCost
      : null,
    costLabel: detail.totalEstimatedCost && detail.totalEstimatedCost > 0
      ? formatCostLabel(detail.totalEstimatedCost)
      : null,
    canRetry: detail.canRetry,
    detailAvailable: detail.detailAvailable,
  }
}

async function buildMutationAcceptedResponse(
  response: RawContentMutationDetailResponseWire | RawContentMutationDetailResponse,
): Promise<DocumentMutationAccepted> {
  const normalized = normalizeContentMutationDetailResponse(response)
  return {
    accepted: true,
    operation: normalized.mutation.operation_kind,
    trackId: normalized.async_operation_id ?? normalized.job_id,
    revisionId:
      normalized.items.find((item) => item.result_revision_id !== null)?.result_revision_id ?? null,
    mutationId: normalized.mutation.id,
    attemptNo: normalized.job_id ? 1 : null,
  }
}

export async function appendDocumentItem(
  libraryId: string,
  id: string,
  content: string,
): Promise<DocumentMutationAccepted> {
  const checksum = await readTextChecksum(content)
  const response = await unwrap(
    apiHttp.post<RawContentMutationDetailResponseWire>(`/content/documents/${id}/append`, {
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
    apiHttp.post<RawContentMutationDetailResponseWire>(
      `/content/documents/${id}/replace`,
      formData,
    ),
  )
  return buildMutationAcceptedResponse(response)
}

export async function downloadDocumentExtractedText(id: string): Promise<Blob> {
  const chunks = await unwrap(
    apiHttp.get<RawChunkSummary[]>('/chunks', {
      params: { documentId: id },
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
