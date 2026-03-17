import type {
  DocumentAccountingStatus,
  DocumentActivityStatus,
  DocumentAttemptGroup,
  DocumentDetail,
  DocumentMutationAccepted,
  DocumentUploadFailure,
  DocumentRow,
  DocumentsSurfaceResponse,
  DocumentMutationStatus,
  UploadRejectionDetails,
  UploadDocumentsResponse,
} from 'src/models/ui/documents'
import { ApiClientError, apiHttp, unwrap } from './http'

interface RawDocumentRow {
  id: string
  logical_document_id: string | null
  file_name: string
  file_type: string
  file_size_label: string
  uploaded_at: string
  library_name: string
  stage: string
  status: 'queued' | 'processing' | 'ready' | 'ready_no_graph' | 'failed'
  progress_percent: number | null
  activity_status?: 'queued' | 'active' | 'blocked' | 'retrying' | 'stalled' | 'ready' | 'failed'
  last_activity_at?: string | null
  stalled_reason?: string | null
  active_revision_no: number | null
  active_revision_kind: string | null
  latest_attempt_no: number
  accounting_status: DocumentAccountingStatus
  total_estimated_cost: number | null
  currency: string | null
  partial_history: boolean
  partial_history_reason: string | null
  mutation: {
    kind: string | null
    status: DocumentMutationStatus | null
    warning: string | null
  }
  can_retry: boolean
  can_append: boolean
  can_replace: boolean
  can_remove: boolean
  detail_available: boolean
  chunk_count?: number | null
  graph_node_count?: number | null
  graph_edge_count?: number | null
}

interface RawDocumentsSurfaceResponse {
  accepted_formats: string[]
  max_size_mb: number
  graph_status: 'empty' | 'building' | 'ready' | 'partial' | 'failed' | 'stale'
  graph_warning: string | null
  rebuild_backlog_count: number
  counters: {
    queued: number
    processing: number
    ready: number
    ready_no_graph: number
    failed: number
  }
  filters: {
    statuses: ('queued' | 'processing' | 'ready' | 'ready_no_graph' | 'failed')[]
    file_types: string[]
  }
  rows: RawDocumentRow[]
}

interface RawDocumentDetail {
  id: string
  logical_document_id: string | null
  file_name: string
  file_type: string
  file_size_label: string
  uploaded_at: string
  library_name: string
  stage: string
  status: 'queued' | 'processing' | 'ready' | 'ready_no_graph' | 'failed'
  progress_percent: number | null
  activity_status?: DocumentActivityStatus
  last_activity_at?: string | null
  stalled_reason?: string | null
  active_revision_no: number | null
  active_revision_kind: string | null
  active_revision_status: string | null
  latest_attempt_no: number
  accounting_status: DocumentAccountingStatus
  total_estimated_cost: number | null
  currency: string | null
  partial_history: boolean
  partial_history_reason: string | null
  mutation: {
    kind: string | null
    status: DocumentMutationStatus | null
    warning: string | null
  }
  requested_by: string | null
  error_message: string | null
  summary: string
  graph_node_id: string | null
  can_download_text: boolean
  can_append: boolean
  can_replace: boolean
  can_remove: boolean
  extracted_stats: {
    chunk_count: number | null
    document_id: string | null
    checksum: string | null
    page_count: number | null
    extraction_kind: string | null
    warnings: string[]
  }
  graph_stats: {
    node_count: number
    edge_count: number
    evidence_count: number
  }
  processing_history: {
    attempt_no: number
    status: string
    stage: string
    error_message: string | null
    started_at: string
    finished_at: string | null
  }[]
  revision_history: {
    id: string
    revision_no: number
    revision_kind: string
    status: string
    source_file_name: string
    appended_text_excerpt: string | null
    accepted_at: string
    activated_at: string | null
    superseded_at: string | null
    is_active: boolean
  }[]
  attempts: {
    attempt_no: number
    revision_no: number | null
    revision_id: string | null
    attempt_kind: string | null
    status: string
    activity_status?: 'queued' | 'active' | 'blocked' | 'retrying' | 'stalled' | 'ready' | 'failed'
    last_activity_at?: string | null
    queue_elapsed_ms: number | null
    total_elapsed_ms: number | null
    started_at: string | null
    finished_at: string | null
    partial_history: boolean
    partial_history_reason: string | null
    summary: {
      total_estimated_cost: number | null
      currency: string | null
      priced_stage_count: number
      unpriced_stage_count: number
      accounting_status: DocumentAccountingStatus
    }
    benchmarks: {
      stage: string
      status: string
      message: string | null
      provider_kind: string | null
      model_name: string | null
      started_at: string
      finished_at: string | null
      elapsed_ms: number | null
      accounting: {
        pricing_status: string
        usage_event_id: string | null
        cost_ledger_id: string | null
        pricing_catalog_entry_id: string | null
        estimated_cost: number | null
        currency: string | null
        attribution_source?: 'stage_native' | 'reconciled' | null
      } | null
    }[]
  }[]
}

interface RawUploadDocumentsResponse {
  accepted_rows: RawDocumentRow[]
}

interface RawDocumentMutationAccepted {
  accepted: boolean
  operation: string
  track_id?: string | null
  trackId?: string | null
  revision_id?: string | null
  revisionId?: string | null
  mutation_id?: string | null
  mutationId?: string | null
  attempt_no?: number | null
  attemptNo?: number | null
}

const STALLED_ACTIVITY_AFTER_MS = 180_000

function pick<T>(snake: T | undefined, camel: T | undefined): T | undefined {
  return snake ?? camel
}

function readString(record: Record<string, unknown>, key: string): string | null {
  const value = record[key]
  return typeof value === 'string' ? value : null
}

function readNumber(record: Record<string, unknown>, key: string): number | null {
  const value = record[key]
  return typeof value === 'number' ? value : null
}

function normalizeUploadRejectionDetails(details: unknown): UploadRejectionDetails | null {
  if (!details || typeof details !== 'object') {
    return null
  }
  const record = details as Record<string, unknown>
  return {
    fileName: readString(record, 'fileName'),
    detectedFormat: readString(record, 'detectedFormat'),
    mimeType: readString(record, 'mimeType'),
    fileSizeBytes: readNumber(record, 'fileSizeBytes'),
    uploadLimitMb: readNumber(record, 'uploadLimitMb'),
    rejectionCause: readString(record, 'rejectionCause'),
    operatorAction: readString(record, 'operatorAction'),
  }
}

export function normalizeDocumentUploadFailure(
  file: File,
  error: unknown,
): DocumentUploadFailure {
  const details =
    error instanceof ApiClientError ? normalizeUploadRejectionDetails(error.details) : null
  const message =
    error instanceof Error ? error.message : 'Failed to upload document'

  return {
    fileName: details?.fileName ?? file.name,
    message,
    errorKind: error instanceof ApiClientError ? error.errorKind : null,
    detectedFormat: details?.detectedFormat ?? null,
    mimeType: details?.mimeType ?? (file.type || null),
    fileSizeBytes: details?.fileSizeBytes ?? file.size,
    uploadLimitMb: details?.uploadLimitMb ?? null,
    rejectionCause: details?.rejectionCause ?? null,
    operatorAction: details?.operatorAction ?? null,
  }
}

function deriveActivityStatus(
  status: string,
  explicitActivityStatus?: DocumentActivityStatus,
  lastActivityAt?: string | null,
): DocumentActivityStatus {
  if (explicitActivityStatus) {
    return explicitActivityStatus
  }
  if (status === 'queued') {
    if (lastActivityAt) {
      const lastSeenAt = Date.parse(lastActivityAt)
      if (!Number.isNaN(lastSeenAt) && Date.now() - lastSeenAt >= STALLED_ACTIVITY_AFTER_MS) {
        return 'stalled'
      }
    }
    return 'queued'
  }
  if (status === 'processing') {
    return 'active'
  }
  if (status === 'ready' || status === 'ready_no_graph') {
    return 'ready'
  }
  if (status === 'failed') {
    return 'failed'
  }
  return 'active'
}

function deriveStalledReason(
  activityStatus: DocumentActivityStatus,
  explicitReason: string | null | undefined,
  lastActivityAt: string | null,
): string | null {
  if (explicitReason) {
    return explicitReason
  }
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

function mapRow(row: RawDocumentRow): DocumentRow {
  const lastActivityAt = row.last_activity_at ?? null
  const activityStatus = deriveActivityStatus(row.status, row.activity_status, lastActivityAt)
  return {
    id: row.id,
    logicalDocumentId: row.logical_document_id,
    fileName: row.file_name,
    fileType: row.file_type,
    fileSizeLabel: row.file_size_label,
    uploadedAt: row.uploaded_at,
    libraryName: row.library_name,
    stage: row.stage,
    status: row.status,
    progressPercent: row.progress_percent,
    activityStatus,
    lastActivityAt,
    stalledReason: deriveStalledReason(activityStatus, row.stalled_reason, lastActivityAt),
    chunkCount: row.chunk_count ?? null,
    graphNodeCount: row.graph_node_count ?? null,
    graphEdgeCount: row.graph_edge_count ?? null,
    activeRevisionNo: row.active_revision_no,
    activeRevisionKind: row.active_revision_kind,
    latestAttemptNo: row.latest_attempt_no,
    accountingStatus: row.accounting_status,
    totalEstimatedCost: row.total_estimated_cost,
    currency: row.currency,
    partialHistory: row.partial_history,
    partialHistoryReason: row.partial_history_reason,
    mutation: {
      kind: row.mutation.kind,
      status: row.mutation.status,
      warning: row.mutation.warning,
    },
    canRetry: row.can_retry,
    canAppend: row.can_append,
    canReplace: row.can_replace,
    canRemove: row.can_remove,
    detailAvailable: row.detail_available,
  }
}

function mapAttemptGroup(attempt: RawDocumentDetail['attempts'][number]): DocumentAttemptGroup {
  const lastActivityAt = attempt.last_activity_at ?? attempt.finished_at ?? attempt.started_at ?? null
  const activityStatus = deriveActivityStatus(
    attempt.status,
    attempt.activity_status,
    lastActivityAt,
  )
  return {
    attemptNo: attempt.attempt_no,
    revisionNo: attempt.revision_no,
    revisionId: attempt.revision_id,
    attemptKind: attempt.attempt_kind,
    status: attempt.status,
    activityStatus,
    lastActivityAt,
    queueElapsedMs: attempt.queue_elapsed_ms,
    totalElapsedMs: attempt.total_elapsed_ms,
    startedAt: attempt.started_at,
    finishedAt: attempt.finished_at,
    partialHistory: attempt.partial_history,
    partialHistoryReason: attempt.partial_history_reason,
    summary: {
      totalEstimatedCost: attempt.summary.total_estimated_cost,
      currency: attempt.summary.currency,
      pricedStageCount: attempt.summary.priced_stage_count,
      unpricedStageCount: attempt.summary.unpriced_stage_count,
      accountingStatus: attempt.summary.accounting_status,
    },
    benchmarks: attempt.benchmarks.map((benchmark) => ({
      stage: benchmark.stage,
      status: benchmark.status,
      message: benchmark.message,
      providerKind: benchmark.provider_kind,
      modelName: benchmark.model_name,
      startedAt: benchmark.started_at,
      finishedAt: benchmark.finished_at,
      elapsedMs: benchmark.elapsed_ms,
      accounting: benchmark.accounting
          ? {
            pricingStatus: benchmark.accounting.pricing_status,
            usageEventId: benchmark.accounting.usage_event_id,
            costLedgerId: benchmark.accounting.cost_ledger_id,
            pricingCatalogEntryId: benchmark.accounting.pricing_catalog_entry_id,
            estimatedCost: benchmark.accounting.estimated_cost,
            currency: benchmark.accounting.currency,
            attributionSource: benchmark.accounting.attribution_source ?? null,
          }
        : null,
    })),
  }
}

function mapMutationAccepted(response: RawDocumentMutationAccepted): DocumentMutationAccepted {
  return {
    accepted: response.accepted,
    operation: response.operation,
    trackId: pick(response.track_id, response.trackId) ?? null,
    revisionId: pick(response.revision_id, response.revisionId) ?? null,
    mutationId: pick(response.mutation_id, response.mutationId) ?? null,
    attemptNo: pick(response.attempt_no, response.attemptNo) ?? null,
  }
}

function mapDetail(detail: RawDocumentDetail): DocumentDetail {
  let lastActivityAt = detail.last_activity_at ?? null
  if (lastActivityAt === null && detail.processing_history.length > 0) {
    const latestProcessingItem = detail.processing_history[0]
    lastActivityAt = latestProcessingItem.finished_at ?? latestProcessingItem.started_at
  }
  const activityStatus = deriveActivityStatus(
    detail.status,
    detail.activity_status,
    lastActivityAt,
  )
  return {
    id: detail.id,
    logicalDocumentId: detail.logical_document_id,
    fileName: detail.file_name,
    fileType: detail.file_type,
    fileSizeLabel: detail.file_size_label,
    uploadedAt: detail.uploaded_at,
    libraryName: detail.library_name,
    stage: detail.stage,
    status: detail.status,
    progressPercent: detail.progress_percent,
    activityStatus,
    lastActivityAt,
    stalledReason: deriveStalledReason(activityStatus, detail.stalled_reason ?? detail.error_message, lastActivityAt),
    activeRevisionNo: detail.active_revision_no,
    activeRevisionKind: detail.active_revision_kind,
    activeRevisionStatus: detail.active_revision_status,
    latestAttemptNo: detail.latest_attempt_no,
    accountingStatus: detail.accounting_status,
    totalEstimatedCost: detail.total_estimated_cost,
    currency: detail.currency,
    partialHistory: detail.partial_history,
    partialHistoryReason: detail.partial_history_reason,
    mutation: {
      kind: detail.mutation.kind,
      status: detail.mutation.status,
      warning: detail.mutation.warning,
    },
    requestedBy: detail.requested_by,
    errorMessage: detail.error_message,
    summary: detail.summary,
    graphNodeId: detail.graph_node_id,
    canDownloadText: detail.can_download_text,
    canAppend: detail.can_append,
    canReplace: detail.can_replace,
    canRemove: detail.can_remove,
    extractedStats: {
      chunkCount: detail.extracted_stats.chunk_count,
      documentId: detail.extracted_stats.document_id,
      checksum: detail.extracted_stats.checksum,
      pageCount: detail.extracted_stats.page_count,
      extractionKind: detail.extracted_stats.extraction_kind,
      warnings: detail.extracted_stats.warnings,
    },
    graphStats: {
      nodeCount: detail.graph_stats.node_count,
      edgeCount: detail.graph_stats.edge_count,
      evidenceCount: detail.graph_stats.evidence_count,
    },
    revisionHistory: detail.revision_history.map((item) => ({
      id: item.id,
      revisionNo: item.revision_no,
      revisionKind: item.revision_kind,
      status: item.status,
      sourceFileName: item.source_file_name,
      appendedTextExcerpt: item.appended_text_excerpt,
      acceptedAt: item.accepted_at,
      activatedAt: item.activated_at,
      supersededAt: item.superseded_at,
      isActive: item.is_active,
    })),
    processingHistory: detail.processing_history.map((item) => ({
      attemptNo: item.attempt_no,
      status: item.status,
      stage: item.stage,
      errorMessage: item.error_message,
      startedAt: item.started_at,
      finishedAt: item.finished_at,
    })),
    attempts: detail.attempts.map(mapAttemptGroup),
  }
}

export async function fetchDocumentsSurface(): Promise<DocumentsSurfaceResponse> {
  const response = await unwrap(apiHttp.get<RawDocumentsSurfaceResponse>('/ui/documents/surface'))
  const accountingStatuses: DocumentAccountingStatus[] = Array.from(
    new Set(response.rows.map((row) => row.accounting_status)),
  ).sort()
  const mutationStatuses: DocumentMutationStatus[] = Array.from(
    new Set(
      response.rows
        .map((row) => row.mutation.status)
        .filter((value): value is DocumentMutationStatus => value !== null),
    ),
  ).sort()

  return {
    acceptedFormats: response.accepted_formats,
    maxSizeMb: response.max_size_mb,
    graphStatus: response.graph_status,
    graphWarning: response.graph_warning,
    rebuildBacklogCount: response.rebuild_backlog_count,
    counters: {
      queued: response.counters.queued,
      processing: response.counters.processing,
      ready: response.counters.ready,
      readyNoGraph: response.counters.ready_no_graph,
      failed: response.counters.failed,
    },
    filters: {
      statuses: response.filters.statuses,
      fileTypes: response.filters.file_types,
      accountingStatuses,
      mutationStatuses,
    },
    rows: response.rows.map(mapRow),
  }
}

export async function uploadDocument(file: File): Promise<DocumentRow> {
  const formData = new FormData()
  formData.append('file', file)

  const response = await unwrap(
    apiHttp.post<RawUploadDocumentsResponse>('/ui/documents/upload', formData),
  )
  if (response.accepted_rows.length === 0) {
    throw new Error('Upload was accepted without a document row')
  }
  return mapRow(response.accepted_rows[0])
}

export async function uploadDocuments(files: File[]): Promise<UploadDocumentsResponse> {
  const formData = new FormData()
  for (const file of files) {
    formData.append('files', file)
  }

  const response = await unwrap(
    apiHttp.post<RawUploadDocumentsResponse>('/ui/documents/upload', formData),
  )

  return {
    acceptedRows: response.accepted_rows.map(mapRow),
  }
}

export async function fetchDocumentDetail(id: string): Promise<DocumentDetail> {
  return mapDetail(await unwrap(apiHttp.get<RawDocumentDetail>(`/ui/documents/${id}`)))
}

export async function retryDocumentItem(id: string): Promise<DocumentRow> {
  return mapRow(await unwrap(apiHttp.post<RawDocumentRow>(`/ui/documents/${id}/retry`)))
}

export async function reprocessDocumentItem(id: string): Promise<void> {
  await unwrap(apiHttp.post<RawDocumentRow>(`/ui/documents/${id}/reprocess`))
}

export async function deleteDocumentItem(id: string): Promise<void> {
  await unwrap(apiHttp.delete<{ ok: boolean }>(`/ui/documents/${id}`))
}

export async function appendDocumentItem(
  libraryId: string,
  id: string,
  content: string,
): Promise<DocumentMutationAccepted> {
  return mapMutationAccepted(
    await unwrap(
      apiHttp.post<RawDocumentMutationAccepted>(
        `/runtime/libraries/${libraryId}/documents/${id}/append`,
        { content },
      ),
    ),
  )
}

export async function replaceDocumentItem(
  libraryId: string,
  id: string,
  file: File,
): Promise<DocumentMutationAccepted> {
  const formData = new FormData()
  formData.append('file', file)

  return mapMutationAccepted(
    await unwrap(
      apiHttp.post<RawDocumentMutationAccepted>(
        `/runtime/libraries/${libraryId}/documents/${id}/replace`,
        formData,
      ),
    ),
  )
}

export async function downloadDocumentExtractedText(id: string): Promise<Blob> {
  const response = await apiHttp.get<Blob>(`/ui/documents/${id}/content`, {
    responseType: 'blob',
  })
  return response.data
}
