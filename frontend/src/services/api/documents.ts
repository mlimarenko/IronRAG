import type {
  CanonicalDocumentIdentity,
  CanonicalDocumentMutation,
  CanonicalDocumentMutationItem,
  CanonicalDocumentRevision,
  CanonicalIngestAttempt,
  CanonicalIngestJob,
  CanonicalIngestStageEvent,
  CreateWebIngestRunInput,
  DocumentAccountingStatus,
  DocumentActivityStatus,
  DocumentAttemptGroup,
  DocumentAttemptSummary,
  DocumentCollectionAccountingSummary,
  DocumentCollectionDiagnostics,
  DocumentCollectionWarning,
  DocumentCollectionGraphThroughputSummary,
  DocumentCollectionProgressCounters,
  DocumentCollectionSettlementSummary,
  DocumentDetail,
  DocumentGraphHealthSummary,
  DocumentGraphThroughputSummary,
  DocumentGraphStats,
  DocumentMutationAccepted,
  DocumentMutationState,
  DocumentGraphCoverageKind,
  DocumentPreparationReadinessKind,
  DocumentPreparationSummary,
  DocumentPreparedSegmentKind,
  DocumentRevisionHistoryItem,
  DocumentRowSummary,
  DocumentStatus,
  DocumentSummaryCounters,
  DocumentTechnicalFactKind,
  DocumentsWorkspaceDiagnosticChip,
  DocumentsWorkspaceNotice,
  DocumentsWorkspacePrimarySummary,
  LibraryGraphCoverageSummary,
  LibraryReadinessSummary,
  DocumentsWorkspaceSummary,
  DocumentsSurfaceResponse,
  DocumentUploadFailure,
  DocumentCostEntry,
  LibraryCostSummary,
  PreparedSegmentRow,
  TechnicalFactRow,
  UploadDocumentsResponse,
  UploadRejectionDetails,
  WebDiscoveredPage,
  WebClassificationReason,
  WebHostClassification,
  WebIngestRun,
  WebIngestRunReceipt,
  WebIngestRunSummary,
  WebRunCounts,
  WebRunFailureCode,
  WebPageProvenance,
} from 'src/models/ui/documents'
import type { DashboardRecentDocument } from 'src/models/ui/dashboard'
import { DOCUMENT_UPLOAD_FORMAT_TOKENS, inferDocumentFileType } from 'src/models/ui/documentFormats'
import type { GraphCanonicalSummary } from 'src/models/ui/graph'
import { i18n } from 'src/lib/i18n'
import { useShellStore } from 'src/stores/shell'
import { ApiClientError, apiHttp, unwrap } from './http'

type RawRow = Record<string, unknown>

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

interface RawContentDocumentReadinessSummary {
  documentId: string
  activeRevisionId: string | null
  readinessKind: string
  activityStatus?: string | null
  stalledReason?: string | null
  preparationState: string
  graphCoverageKind: string
  typedFactCoverage: number | null
  lastMutationId: string | null
  lastJobStage: string | null
  updatedAt: string
}

interface RawKnowledgeLibrarySummaryResponse {
  libraryId: string
  documentCountsByReadiness?: Record<string, number> | null
  graphReadyDocumentCount?: number | null
  graphSparseDocumentCount?: number | null
  typedFactDocumentCount?: number | null
  updatedAt?: string | null
  latestGeneration?: RawRow | null
}

interface RawWebPageProvenance {
  runId: string | null
  candidateId: string | null
  sourceUri: string | null
  canonicalUrl: string | null
}

export interface LibraryKnowledgeSummaryResponse {
  libraryId: string
  readinessSummary: LibraryReadinessSummary
  graphCoverage: LibraryGraphCoverageSummary
  latestGeneration: RawRow | null
}

export interface LibraryKnowledgeSummarySnapshot {
  summary: LibraryKnowledgeSummaryResponse | null
  unavailable: boolean
}

export const KNOWLEDGE_SUMMARY_UNAVAILABLE_WARNING =
  'The canonical knowledge summary is temporarily unavailable.'

export interface LibraryKnowledgeSummaryProjection {
  summary: LibraryKnowledgeSummaryResponse | null
  warning: string | null
}

interface RawWebRunCounts {
  discovered?: number | null
  eligible?: number | null
  processed?: number | null
  queued?: number | null
  processing?: number | null
  duplicates?: number | null
  excluded?: number | null
  blocked?: number | null
  failed?: number | null
  canceled?: number | null
}

interface RawWebIngestRunReceipt {
  runId: string
  libraryId: string
  mode: string
  runState: string
  asyncOperationId?: string | null
  counts?: RawWebRunCounts | null
  failureCode?: string | null
  cancelRequestedAt?: string | null
}

interface RawWebIngestRunSummary {
  runId: string
  libraryId: string
  mode: string
  boundaryPolicy: string
  maxDepth: number
  maxPages: number
  runState: string
  seedUrl: string
  counts: RawWebRunCounts
  lastActivityAt?: string | null
}

interface RawWebIngestRun extends RawWebIngestRunSummary {
  mutationId: string
  asyncOperationId?: string | null
  workspaceId: string
  normalizedSeedUrl: string
  requestedByPrincipalId?: string | null
  requestedAt: string
  completedAt?: string | null
  failureCode?: string | null
  cancelRequestedAt?: string | null
}

interface RawWebDiscoveredPage {
  candidateId: string
  runId: string
  discoveredUrl?: string | null
  normalizedUrl: string
  finalUrl?: string | null
  canonicalUrl?: string | null
  depth: number
  referrerCandidateId?: string | null
  hostClassification: string
  candidateState: string
  classificationReason?: string | null
  contentType?: string | null
  httpStatus?: number | null
  discoveredAt: string
  updatedAt: string
  documentId?: string | null
  resultRevisionId?: string | null
  mutationItemId?: string | null
}

const WEB_CLASSIFICATION_REASONS = [
  'seed_accepted',
  'duplicate_canonical_url',
  'outside_boundary_policy',
  'exceeded_max_depth',
  'exceeded_max_pages',
  'unsupported_scheme',
  'invalid_url',
  'inaccessible',
  'unsupported_content',
  'cancel_requested',
] as const satisfies readonly WebClassificationReason[]

const WEB_RUN_FAILURE_CODES = [
  'inaccessible',
  'invalid_url',
  'unsupported_content',
  'web_discovery_failed',
  'web_snapshot_persist_failed',
  'web_snapshot_missing',
  'web_snapshot_missing_final_url',
  'web_snapshot_unavailable',
  'web_capture_materialization_failed',
  'recursive_crawl_failed',
] as const satisfies readonly WebRunFailureCode[]

interface RawStructuredOutlineEntry {
  blockId: string
  blockOrdinal: number
  depth: number
  heading: string
  headingTrail: string[]
  sectionPath: string[]
}

interface RawStructuredDocumentRevision {
  revisionId: string
  documentId: string
  workspaceId: string
  libraryId: string
  preparationState: string
  normalizationProfile: string
  sourceFormat: string
  languageCode: string | null
  blockCount: number
  chunkCount: number
  typedFactCount: number
  outline: RawStructuredOutlineEntry[]
  preparedAt: string
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
  readiness_summary?: RawContentDocumentReadinessSummary | null
  readinessSummary?: RawContentDocumentReadinessSummary | null
  web_page_provenance?: RawWebPageProvenance | null
  webPageProvenance?: RawWebPageProvenance | null
  prepared_revision?: RawStructuredDocumentRevision | null
  preparedRevision?: RawStructuredDocumentRevision | null
  prepared_segment_count?: number | null
  preparedSegmentCount?: number | null
  technical_fact_count?: number | null
  technicalFactCount?: number | null
  pipeline: RawContentDocumentPipelineState
}

interface RawContentDocumentPipelineJob {
  id: string
  workspace_id: string
  library_id: string
  mutation_id: string | null
  async_operation_id: string | null
  job_kind: string
  queue_state: string
  queued_at: string
  available_at: string
  completed_at: string | null
  claimed_at?: string | null
  last_activity_at?: string | null
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
  queue_state: string
  priority: number
  dedupe_key: string | null
  queued_at: string
  available_at: string
  completed_at: string | null
  current_stage?: string | null
  failure_code?: string | null
  retryable?: boolean
}

interface RawChunkSummary {
  id: string
  document_id: string
  project_id: string
  ordinal: number
  content: string
  token_count: number | null
}

interface RawPreparedSegmentListItem {
  segmentId: string
  revisionId: string
  ordinal: number
  blockKind: string
  headingTrail: string[]
  sectionPath: string[]
  pageNumber: number | null
  excerpt: string
}

interface RawStructuredSourceSpan {
  startOffset: number
  endOffset: number
}

interface RawStructuredTableCoordinates {
  rowIndex: number
  columnIndex: number
  rowSpan: number
  columnSpan: number
}

interface RawPreparedSegmentDetail {
  segment: RawPreparedSegmentListItem
  text: string
  normalizedText: string
  sourceSpan: RawStructuredSourceSpan | null
  parentBlockId: string | null
  tableCoordinates: RawStructuredTableCoordinates | null
  codeLanguage: string | null
  supportChunkIds: string[]
}

interface RawPreparedSegmentsPageResponse {
  documentId: string
  revisionId: string | null
  total: number
  offset: number
  limit: number
  items: RawPreparedSegmentDetail[]
}

type RawTechnicalFactValue =
  | {
      valueType: 'text'
      value: string
    }
  | {
      valueType: 'integer'
      value: number
    }

interface RawTechnicalFactQualifier {
  key: string
  value: string
}

interface RawTypedTechnicalFact {
  factId: string
  revisionId: string
  documentId: string
  workspaceId: string
  libraryId: string
  factKind: string
  canonicalValue: RawTechnicalFactValue
  displayValue: string
  qualifiers: RawTechnicalFactQualifier[]
  supportBlockIds: string[]
  supportChunkIds: string[]
  confidence: number | null
  extractionKind: string
  conflictGroupId: string | null
  createdAt: string
}

interface RawTechnicalFactsPageResponse {
  documentId: string
  revisionId: string | null
  total: number
  offset: number
  limit: number
  items: RawTypedTechnicalFact[]
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
    readinessSummary?: RawContentDocumentReadinessSummary | null
    webPageProvenance?: RawWebPageProvenance | null
    preparedRevision?: RawStructuredDocumentRevision | null
    preparedSegmentCount?: number | null
    technicalFactCount?: number | null
    pipeline?: RawContentDocumentPipelineState | null
  }
  const fileName =
    ('file_name' in detail && typeof detail.file_name === 'string'
      ? detail.file_name
      : detail.fileName) ?? detail.document.external_key
  const activeRevision =
    ('active_revision' in detail ? detail.active_revision : undefined) ??
    detail.activeRevision ??
    null
  const readiness = ('readiness' in detail ? detail.readiness : undefined) ?? null
  const readinessSummary =
    ('readiness_summary' in detail ? detail.readiness_summary : undefined) ??
    detail.readinessSummary ??
    null
  const preparedRevision =
    ('prepared_revision' in detail ? detail.prepared_revision : undefined) ??
    detail.preparedRevision ??
    null
  const webPageProvenance =
    ('web_page_provenance' in detail ? detail.web_page_provenance : undefined) ??
    detail.webPageProvenance ??
    null
  const preparedSegmentCount =
    ('prepared_segment_count' in detail ? detail.prepared_segment_count : undefined) ??
    detail.preparedSegmentCount ??
    null
  const technicalFactCount =
    ('technical_fact_count' in detail ? detail.technical_fact_count : undefined) ??
    detail.technicalFactCount ??
    null
  return {
    document: detail.document,
    file_name: fileName,
    head: detail.head ?? null,
    active_revision: activeRevision,
    readiness,
    readiness_summary: readinessSummary,
    web_page_provenance: webPageProvenance,
    prepared_revision: preparedRevision,
    prepared_segment_count: preparedSegmentCount,
    technical_fact_count: technicalFactCount,
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

function humanizeToken(value: string): string {
  return value
    .replaceAll('.', ' ')
    .replaceAll('_', ' ')
    .replaceAll('-', ' ')
    .trim()
    .split(/\s+/)
    .filter((part) => part.length > 0)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(' ')
}

function normalizeFailureMessage(value: string): string {
  return value.replace(/\s+/g, ' ').trim()
}

function findMutationFailureMessage(
  mutation: RawContentMutationDetailResponse | null | undefined,
): string | null {
  if (!mutation) {
    return null
  }
  const failedItem =
    mutation.items.find((item) => item.item_state === 'failed' && item.message?.trim().length) ??
    mutation.items.find((item) => item.message?.trim().length) ??
    null
  return failedItem?.message ? normalizeFailureMessage(failedItem.message) : null
}

function isMissingGraphBindingFailure(value: string | null | undefined): boolean {
  if (!value) {
    return false
  }
  const normalized = value.toLowerCase()
  return (
    normalized.includes('active extract_graph binding is required') ||
    normalized.includes('binding is not configured for library')
  )
}

function buildDocumentFailureMessage(
  mutation: RawContentMutationDetailResponse | null,
  job: RawIngestJob | null,
  status: DocumentStatus,
): string | null {
  const mutationMessage = findMutationFailureMessage(mutation)
  if (isMissingGraphBindingFailure(mutationMessage)) {
    return i18n.global.t('documents.details.failureNeedsGraphBinding')
  }

  const normalizedMessage = mutationMessage ?? normalizeFailureMessage(job?.failure_code ?? '')
  if (normalizedMessage) {
    if (normalizedMessage === 'canonical_pipeline_failed') {
      return i18n.global.t('documents.details.failureGeneric')
    }
    return normalizedMessage
  }

  if (status === 'failed') {
    return i18n.global.t('documents.details.failureGeneric')
  }
  return null
}

function buildDocumentFailureActionMessage(
  mutation: RawContentMutationDetailResponse | null,
  job: RawIngestJob | null,
): string | null {
  const message = findMutationFailureMessage(mutation) ?? job?.failure_code ?? null
  if (isMissingGraphBindingFailure(message)) {
    return i18n.global.t('documents.details.failureNeedsGraphBindingAction')
  }
  return null
}

function mapPreparationReadinessKind(
  value: string | null | undefined,
): DocumentPreparationReadinessKind {
  switch (value) {
    case 'readable':
    case 'graph_sparse':
    case 'graph_ready':
    case 'failed':
      return value
    case 'processing':
    default:
      return 'processing'
  }
}

function mapGraphCoverageKind(value: string | null | undefined): DocumentGraphCoverageKind {
  switch (value) {
    case 'graph_sparse':
    case 'graph_ready':
    case 'failed':
      return value
    case 'processing':
    default:
      return 'processing'
  }
}

function emptyReadinessCounters(): DocumentSummaryCounters {
  return {
    processing: 0,
    readable: 0,
    graphSparse: 0,
    graphReady: 0,
    failed: 0,
  }
}

export function emptyLibraryReadinessSummary(libraryId = ''): LibraryReadinessSummary {
  return {
    libraryId,
    documentCountsByReadiness: emptyReadinessCounters(),
    updatedAt: null,
  }
}

export function emptyLibraryGraphCoverage(libraryId = ''): LibraryGraphCoverageSummary {
  return {
    libraryId,
    graphReadyDocumentCount: 0,
    graphSparseDocumentCount: 0,
    typedFactDocumentCount: 0,
    lastGenerationId: null,
    updatedAt: null,
  }
}

export function emptyLibraryKnowledgeSummary(libraryId = ''): LibraryKnowledgeSummaryResponse {
  return {
    libraryId,
    readinessSummary: emptyLibraryReadinessSummary(libraryId),
    graphCoverage: emptyLibraryGraphCoverage(libraryId),
    latestGeneration: null,
  }
}

function mapReadinessCounterKey(value: string): keyof DocumentSummaryCounters | null {
  switch (value.trim().toLowerCase()) {
    case 'processing':
      return 'processing'
    case 'readable':
      return 'readable'
    case 'graph_sparse':
      return 'graphSparse'
    case 'graph_ready':
      return 'graphReady'
    case 'failed':
      return 'failed'
    default:
      return null
  }
}

function normalizeReadinessCounters(
  counts:
    | DocumentSummaryCounters
    | Partial<Record<keyof DocumentSummaryCounters, number>>
    | Partial<Record<string, number>>
    | null
    | undefined,
): DocumentSummaryCounters {
  const normalized = emptyReadinessCounters()
  for (const [key, rawValue] of Object.entries(counts ?? {})) {
    const targetKey = mapReadinessCounterKey(key)
    if (!targetKey) {
      continue
    }
    normalized[targetKey] = Number.isFinite(rawValue) ? Math.max(0, Number(rawValue)) : 0
  }
  return normalized
}

export function buildEmptyLibraryKnowledgeSummary(libraryId = ''): LibraryKnowledgeSummaryResponse {
  return {
    libraryId,
    readinessSummary: {
      libraryId,
      documentCountsByReadiness: emptyReadinessCounters(),
      updatedAt: null,
    },
    graphCoverage: {
      libraryId,
      graphReadyDocumentCount: 0,
      graphSparseDocumentCount: 0,
      typedFactDocumentCount: 0,
      lastGenerationId: null,
      updatedAt: null,
    },
    latestGeneration: null,
  }
}

function mapPreparedSegmentKind(value: string): DocumentPreparedSegmentKind {
  switch (value) {
    case 'heading':
    case 'paragraph':
    case 'list_item':
    case 'table':
    case 'table_row':
    case 'code_block':
    case 'endpoint_block':
    case 'quote_block':
    case 'metadata_block':
      return value
    default:
      return 'paragraph'
  }
}

function mapTechnicalFactKind(value: string): DocumentTechnicalFactKind {
  switch (value) {
    case 'url':
    case 'endpoint_path':
    case 'http_method':
    case 'port':
    case 'parameter_name':
    case 'status_code':
    case 'protocol':
    case 'auth_rule':
    case 'identifier':
      return value
    default:
      return 'identifier'
  }
}

function technicalFactValueLabel(value: RawTechnicalFactValue): string {
  return typeof value.value === 'number' ? String(value.value) : value.value
}

function buildPreparationSummary(
  detail: RawContentDocumentDetailResponse,
): DocumentPreparationSummary | null {
  const summary = detail.readiness_summary ?? null
  const preparedRevision = detail.prepared_revision ?? null
  if (!summary && !preparedRevision) {
    return null
  }
  return {
    readinessKind: mapPreparationReadinessKind(summary?.readinessKind),
    preparationState: summary?.preparationState ?? preparedRevision?.preparationState ?? 'pending',
    graphCoverageKind: mapGraphCoverageKind(summary?.graphCoverageKind),
    typedFactCoverage: summary?.typedFactCoverage ?? null,
    lastProcessingStage: summary?.lastJobStage ?? null,
    updatedAt: summary?.updatedAt ?? preparedRevision?.preparedAt ?? null,
    sourceFormat: preparedRevision?.sourceFormat ?? null,
    normalizationProfile: preparedRevision?.normalizationProfile ?? null,
    preparedSegmentCount:
      detail.prepared_segment_count ?? detail.prepared_revision?.blockCount ?? 0,
    technicalFactCount:
      detail.technical_fact_count ?? detail.prepared_revision?.typedFactCount ?? 0,
  }
}

export function mapLibraryKnowledgeSummary(
  libraryId: string,
  response: RawKnowledgeLibrarySummaryResponse,
): LibraryKnowledgeSummaryResponse {
  const resolvedLibraryId = response.libraryId || libraryId
  const latestGeneration = response.latestGeneration ?? null
  const lastGenerationIdValue =
    latestGeneration?.['generationId'] ??
    latestGeneration?.['generation_id'] ??
    latestGeneration?.['id'] ??
    null
  const lastGenerationId = lastGenerationIdValue == null ? null : String(lastGenerationIdValue)
  const updatedAt = response.updatedAt ?? null

  return {
    libraryId: resolvedLibraryId,
    readinessSummary: {
      libraryId: resolvedLibraryId,
      documentCountsByReadiness: normalizeReadinessCounters(response.documentCountsByReadiness),
      updatedAt,
    },
    graphCoverage: {
      libraryId: resolvedLibraryId,
      graphReadyDocumentCount: Math.max(0, Number(response.graphReadyDocumentCount ?? 0)),
      graphSparseDocumentCount: Math.max(0, Number(response.graphSparseDocumentCount ?? 0)),
      typedFactDocumentCount: Math.max(0, Number(response.typedFactDocumentCount ?? 0)),
      lastGenerationId,
      updatedAt,
    },
    latestGeneration,
  }
}

function resolveContextLibraryId(libraryId?: string | null): string | null {
  return libraryId ?? useShellStore().context?.activeLibrary.id ?? null
}

export async function resolveLibraryKnowledgeSummaryProjection(
  libraryId?: string | null,
  fallback: LibraryKnowledgeSummaryResponse | null = null,
): Promise<LibraryKnowledgeSummaryProjection> {
  const resolvedLibraryId = resolveContextLibraryId(libraryId)
  if (!resolvedLibraryId) {
    return {
      summary: fallback,
      warning: null,
    }
  }

  try {
    return {
      summary: await fetchLibraryKnowledgeSummary(resolvedLibraryId),
      warning: null,
    }
  } catch {
    return {
      summary: fallback ?? buildEmptyLibraryKnowledgeSummary(resolvedLibraryId),
      warning: KNOWLEDGE_SUMMARY_UNAVAILABLE_WARNING,
    }
  }
}

function mapPreparedSegments(items: RawPreparedSegmentDetail[]): PreparedSegmentRow[] {
  return items.map((item) => ({
    id: item.segment.segmentId,
    revisionId: item.segment.revisionId,
    ordinal: item.segment.ordinal,
    kind: mapPreparedSegmentKind(item.segment.blockKind),
    headingTrail: item.segment.headingTrail,
    sectionPath: item.segment.sectionPath,
    excerpt: item.segment.excerpt,
    text: item.text,
    normalizedText: item.normalizedText,
    location: {
      pageNumber: item.segment.pageNumber,
      startOffset: item.sourceSpan?.startOffset ?? null,
      endOffset: item.sourceSpan?.endOffset ?? null,
      supportChunkCount: item.supportChunkIds.length,
    },
    parentSegmentId: item.parentBlockId,
    codeLanguage: item.codeLanguage,
    tableCoordinates: item.tableCoordinates
      ? {
          rowIndex: item.tableCoordinates.rowIndex,
          columnIndex: item.tableCoordinates.columnIndex,
          rowSpan: item.tableCoordinates.rowSpan,
          columnSpan: item.tableCoordinates.columnSpan,
        }
      : null,
    supportChunkIds: item.supportChunkIds,
  }))
}

function mapTechnicalFacts(
  items: RawTypedTechnicalFact[],
  preparedSegments: PreparedSegmentRow[],
): TechnicalFactRow[] {
  const segmentIndex = new Map(
    preparedSegments.map(
      (segment) =>
        [
          segment.id,
          {
            segmentId: segment.id,
            ordinal: segment.ordinal,
            label:
              segment.headingTrail.at(-1) ??
              segment.sectionPath.at(-1) ??
              `${humanizeToken(segment.kind)} #${String(segment.ordinal + 1)}`,
          },
        ] as const,
    ),
  )

  return items.map((item) => ({
    id: item.factId,
    revisionId: item.revisionId,
    documentId: item.documentId,
    kind: mapTechnicalFactKind(item.factKind),
    canonicalValueLabel: technicalFactValueLabel(item.canonicalValue),
    displayValue: item.displayValue,
    qualifiers: item.qualifiers.map((qualifier) => ({
      key: qualifier.key,
      value: qualifier.value,
    })),
    supportChunkIds: item.supportChunkIds,
    supportSegments: item.supportBlockIds
      .map((segmentId) => segmentIndex.get(segmentId))
      .filter((segment): segment is NonNullable<typeof segment> => segment !== undefined),
    confidence: item.confidence,
    extractionKind: item.extractionKind,
    conflictGroupId: item.conflictGroupId,
    createdAt: item.createdAt,
  }))
}

function readinessLabelFor(value: DocumentPreparationReadinessKind): string {
  const key = `documents.readinessKinds.${value}`
  return i18n.global.te(key) ? i18n.global.t(key) : humanizeToken(value)
}

function statusLabelFor(
  status: DocumentStatus,
  preparation: DocumentPreparationSummary | null = null,
): string {
  if (preparation) {
    return readinessLabelFor(preparation.readinessKind)
  }
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

function stageKeyFor(job: RawIngestJob | null, mutationState: string | null = null): string | null {
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

function stageLabelFor(
  job: RawIngestJob | null,
  mutationState: string | null = null,
): string | null {
  const stageKey = stageKeyFor(job, mutationState)
  if (!stageKey) return null
  if (stageKey === 'completed') return null
  return documentStageLabel(stageKey)
}

function progressPercentForState(
  preparation: DocumentPreparationSummary | null,
  status: DocumentStatus,
  activityStatus: DocumentActivityStatus,
  stageKey: string | null,
): number | null {
  const inFlight =
    status === 'queued' ||
    status === 'processing' ||
    activityStatus === 'queued' ||
    activityStatus === 'active' ||
    activityStatus === 'retrying' ||
    activityStatus === 'blocked' ||
    activityStatus === 'stalled'

  switch (preparation?.readinessKind) {
    case 'graph_ready':
      return 100
    case 'graph_sparse':
      return 92
    case 'readable':
      return 78
    case 'failed':
      return null
    default:
      break
  }

  if (status === 'failed') {
    return null
  }

  if (status === 'ready') {
    return 100
  }

  if (status === 'ready_no_graph') {
    return 84
  }

  if (inFlight) {
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
      case 'completed':
        return 96
      default:
        return activityStatus === 'queued' ? 8 : 24
    }
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
      return null
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

function mutationStatusFromState(
  value: string,
): 'accepted' | 'reconciling' | 'completed' | 'failed' {
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

function readinessTextState(
  readiness: RawContentRevisionReadiness | RawKnowledgeRevisionRow,
): string {
  return 'text_state' in readiness ? readiness.text_state : readiness.textState
}

function readinessVectorState(
  readiness: RawContentRevisionReadiness | RawKnowledgeRevisionRow,
): string {
  return 'vector_state' in readiness ? readiness.vector_state : readiness.vectorState
}

function readinessGraphState(
  readiness: RawContentRevisionReadiness | RawKnowledgeRevisionRow,
): string {
  return 'graph_state' in readiness ? readiness.graph_state : readiness.graphState
}

function isFailedReadiness(
  readiness: RawContentRevisionReadiness | RawKnowledgeRevisionRow | null,
): boolean {
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
  return (
    mutationState === 'failed' || mutationState === 'conflicted' || mutationState === 'canceled'
  )
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

function documentStatusFromPreparationState(
  preparation: DocumentPreparationSummary | null,
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

  switch (preparation?.readinessKind) {
    case 'graph_ready':
      return 'ready'
    case 'readable':
    case 'graph_sparse':
      return 'ready_no_graph'
    case 'failed':
      return 'failed'
    case 'processing':
      return queueState === 'queued' ? 'queued' : 'processing'
    default:
      return documentStatusFromCurrentState(readiness, mutationState, queueState)
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

  if ((activityStatus === 'queued' || activityStatus === 'active') && lastActivityAt !== null) {
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

function stalledReason(
  activityStatus: DocumentActivityStatus,
  lastActivityAt: string | null,
): string | null {
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
    failureClass:
      job.queue_state === 'failed' || job.queue_state === 'canceled' ? 'ingest_failed' : null,
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

function buildWebPageProvenance(
  provenance: RawWebPageProvenance | null | undefined,
): WebPageProvenance | null {
  if (!provenance) {
    return null
  }
  return {
    runId: provenance.runId,
    candidateId: provenance.candidateId,
    sourceUri: provenance.sourceUri,
    canonicalUrl: provenance.canonicalUrl,
  }
}

function normalizeWebRunCounts(raw: RawWebRunCounts | null | undefined): WebRunCounts {
  return {
    discovered: raw?.discovered ?? 0,
    eligible: raw?.eligible ?? 0,
    processed: raw?.processed ?? 0,
    queued: raw?.queued ?? 0,
    processing: raw?.processing ?? 0,
    duplicates: raw?.duplicates ?? 0,
    excluded: raw?.excluded ?? 0,
    blocked: raw?.blocked ?? 0,
    failed: raw?.failed ?? 0,
    canceled: raw?.canceled ?? 0,
  }
}

function normalizeWebClassificationReason(
  reason: string | null | undefined,
): WebClassificationReason | null {
  if (!reason || !WEB_CLASSIFICATION_REASONS.includes(reason as WebClassificationReason)) {
    return null
  }
  return reason as WebClassificationReason
}

function normalizeWebRunFailureCode(
  failureCode: string | null | undefined,
): WebRunFailureCode | null {
  if (!failureCode || !WEB_RUN_FAILURE_CODES.includes(failureCode as WebRunFailureCode)) {
    return null
  }
  return failureCode as WebRunFailureCode
}

function normalizeWebHostClassification(value: string | null | undefined): WebHostClassification {
  return value === 'external' ? 'external' : 'same_host'
}

function mapWebIngestRunReceipt(receipt: RawWebIngestRunReceipt): WebIngestRunReceipt {
  return {
    runId: receipt.runId,
    libraryId: receipt.libraryId,
    mode: receipt.mode === 'recursive_crawl' ? 'recursive_crawl' : 'single_page',
    runState:
      receipt.runState === 'discovering' ||
      receipt.runState === 'processing' ||
      receipt.runState === 'completed' ||
      receipt.runState === 'completed_partial' ||
      receipt.runState === 'failed' ||
      receipt.runState === 'canceled'
        ? receipt.runState
        : 'accepted',
    asyncOperationId: receipt.asyncOperationId ?? null,
    counts: normalizeWebRunCounts(receipt.counts),
    failureCode: normalizeWebRunFailureCode(receipt.failureCode),
    cancelRequestedAt: receipt.cancelRequestedAt ?? null,
  }
}

function mapWebIngestRunSummary(run: RawWebIngestRunSummary): WebIngestRunSummary {
  return {
    runId: run.runId,
    libraryId: run.libraryId,
    mode: run.mode === 'recursive_crawl' ? 'recursive_crawl' : 'single_page',
    boundaryPolicy: run.boundaryPolicy === 'allow_external' ? 'allow_external' : 'same_host',
    maxDepth: run.maxDepth,
    maxPages: run.maxPages,
    runState:
      run.runState === 'discovering' ||
      run.runState === 'processing' ||
      run.runState === 'completed' ||
      run.runState === 'completed_partial' ||
      run.runState === 'failed' ||
      run.runState === 'canceled'
        ? run.runState
        : 'accepted',
    seedUrl: run.seedUrl,
    counts: normalizeWebRunCounts(run.counts),
    lastActivityAt: run.lastActivityAt ?? null,
  }
}

function mapWebIngestRun(run: RawWebIngestRun): WebIngestRun {
  const summary = mapWebIngestRunSummary(run)
  return {
    ...summary,
    mutationId: run.mutationId,
    asyncOperationId: run.asyncOperationId ?? null,
    workspaceId: run.workspaceId,
    normalizedSeedUrl: run.normalizedSeedUrl,
    requestedByPrincipalId: run.requestedByPrincipalId ?? null,
    requestedAt: run.requestedAt,
    completedAt: run.completedAt ?? null,
    failureCode: normalizeWebRunFailureCode(run.failureCode),
    cancelRequestedAt: run.cancelRequestedAt ?? null,
  }
}

function mapWebDiscoveredPage(page: RawWebDiscoveredPage): WebDiscoveredPage {
  return {
    candidateId: page.candidateId,
    runId: page.runId,
    discoveredUrl: page.discoveredUrl ?? null,
    normalizedUrl: page.normalizedUrl,
    finalUrl: page.finalUrl ?? null,
    canonicalUrl: page.canonicalUrl ?? null,
    depth: page.depth,
    referrerCandidateId: page.referrerCandidateId ?? null,
    hostClassification: normalizeWebHostClassification(page.hostClassification),
    candidateState:
      page.candidateState === 'eligible' ||
      page.candidateState === 'duplicate' ||
      page.candidateState === 'excluded' ||
      page.candidateState === 'blocked' ||
      page.candidateState === 'queued' ||
      page.candidateState === 'processing' ||
      page.candidateState === 'processed' ||
      page.candidateState === 'failed' ||
      page.candidateState === 'canceled'
        ? page.candidateState
        : 'discovered',
    classificationReason: normalizeWebClassificationReason(page.classificationReason),
    contentType: page.contentType ?? null,
    httpStatus: page.httpStatus ?? null,
    discoveredAt: page.discoveredAt,
    updatedAt: page.updatedAt,
    documentId: page.documentId ?? null,
    resultRevisionId: page.resultRevisionId ?? null,
    mutationItemId: page.mutationItemId ?? null,
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
  return activeRevision?.title ?? revisions[0]?.title ?? detail.file_name
}

function buildDocumentFileType(
  detail: RawContentDocumentDetailResponse,
  activeRevision: RawContentRevision | null,
  revisions: RawContentRevision[],
): string {
  const reference = activeRevision ?? revisions[0]
  if (!reference) {
    return inferDocumentFileType(detail.file_name, null)
  }
  const fileNameReference = reference.source_uri?.replace(/^upload:\/\//, '') ?? detail.file_name
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

function buildAttemptSummary(
  job: RawIngestJob,
  accountingStatus: DocumentAccountingStatus,
): DocumentAttemptSummary {
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

function buildAttemptGroup(
  job: RawIngestJob,
  revision: RawContentRevision | null,
): DocumentAttemptGroup {
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
    queueElapsedMs: elapsedMs(job.queued_at, job.available_at ?? job.completed_at),
    totalElapsedMs: elapsedMs(job.queued_at, job.completed_at ?? new Date().toISOString()),
    startedAt: job.queued_at,
    finishedAt: job.completed_at,
    partialHistory: job.queue_state !== 'completed',
    partialHistoryReason:
      job.queue_state === 'failed' || job.queue_state === 'canceled' ? 'ingest_failed' : null,
    summary,
    benchmarks: [],
  }
}

function buildProcessingHistory(job: RawIngestJob): DocumentDetail['processingHistory'][number] {
  return {
    attemptNo: 1,
    status: job.queue_state,
    stage: stageKeyFor(job) ?? 'accepted',
    errorMessage:
      job.queue_state === 'failed' || job.queue_state === 'canceled'
        ? 'Canonical ingest job failed'
        : null,
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
  const graphReadyAt = 'graphReadyAt' in revision ? revision.graphReadyAt : revision.graph_ready_at

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
  counters: DocumentSummaryCounters,
): DocumentCollectionAccountingSummary {
  const inFlightStageCount = counters.processing
  return {
    totalEstimatedCost: null,
    settledEstimatedCost: null,
    inFlightEstimatedCost: null,
    currency: null,
    promptTokens: 0,
    completionTokens: 0,
    totalTokens: 0,
    pricedStageCount: counters.readable + counters.graphSparse + counters.graphReady,
    unpricedStageCount: counters.processing + counters.failed,
    inFlightStageCount,
    missingStageCount: 0,
    accountingStatus: inFlightStageCount > 0 ? 'in_flight_unsettled' : 'priced',
  }
}

function buildCollectionDiagnostics(
  rows: DocumentRowSummary[],
  counters: DocumentSummaryCounters,
): DocumentCollectionDiagnostics {
  const processing = counters.processing
  const readable = counters.readable
  const graphSparse = counters.graphSparse
  const graphReady = counters.graphReady
  const failed = counters.failed
  const activeBacklogCount = processing
  const progress: DocumentCollectionProgressCounters = {
    accepted: rows.length,
    contentExtracted: readable + graphSparse + graphReady,
    chunked: readable + graphSparse + graphReady,
    embedded: graphSparse + graphReady,
    extractingGraph: processing,
    graphReady,
    ready: graphReady,
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
    activeWriteCount: graphReady,
    retryingWriteCount: processing,
    failedWriteCount: failed,
    pendingNodeWriteCount: activeBacklogCount,
    pendingEdgeWriteCount: activeBacklogCount,
    lastFailureKind: failed > 0 ? 'canonical_ingest_failed' : null,
    lastFailureAt: failed > 0 ? new Date().toISOString() : null,
    isRuntimeReadable: failed === 0 && activeBacklogCount === 0 && graphReady > 0,
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
    queuedCount: 0,
    processingCount: processing,
    pendingGraphCount: readable + graphSparse,
    failedDocumentCount: failed,
    settledAt: settlement.settledAt,
    lastTransitionAt:
      rows.length > 0 ? (rows.map((row) => row.uploadedAt).sort(compareIsoDates)[0] ?? null) : null,
  } satisfies NonNullable<DocumentCollectionDiagnostics['terminalOutcome']>

  const graphThroughput: DocumentCollectionGraphThroughputSummary = {
    trackedDocumentCount: rows.length,
    activeDocumentCount: activeBacklogCount,
    ...buildGraphThroughput(
      activeBacklogCount,
      rows.length,
      readable + graphSparse + graphReady,
      failed > 0 ? 'high' : activeBacklogCount > 0 ? 'elevated' : null,
    ),
  }

  return {
    progress,
    queueBacklogCount: 0,
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
  counters: DocumentSummaryCounters,
  diagnostics: DocumentCollectionDiagnostics,
): DocumentsWorkspaceSummary {
  const backlogCount = counters.processing
  const failed = counters.failed
  const progressCount =
    counters.readable + counters.graphSparse + counters.graphReady + counters.failed
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
      kind: 'processing',
      label: readinessLabelFor('processing'),
      value: String(counters.processing),
    },
    {
      kind: 'readable',
      label: readinessLabelFor('readable'),
      value: String(counters.readable),
    },
    {
      kind: 'graph_sparse',
      label: readinessLabelFor('graph_sparse'),
      value: String(counters.graphSparse),
    },
    {
      kind: 'graph_ready',
      label: readinessLabelFor('graph_ready'),
      value: String(counters.graphReady),
    },
    {
      kind: 'failed',
      label: i18n.global.t('documents.failed'),
      value: String(counters.failed),
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

  if (counters.failed > 0) {
    degradedNotices.push({
      kind: 'failed_work',
      title: i18n.global.t('documents.workspace.notices.pipelineFailed.title'),
      message: i18n.global.t('documents.workspace.notices.pipelineFailed.message', {
        count: counters.failed,
      }),
    })
  }

  if (counters.readable > 0 || counters.graphSparse > 0) {
    informationalNotices.push({
      kind: 'graph_sparse',
      title: i18n.global.t('documents.workspace.notices.graphSparse.title'),
      message: i18n.global.t('documents.workspace.notices.graphSparse.message', {
        readable: counters.readable,
        graphSparse: counters.graphSparse,
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
      counters.failed > 0
        ? 'failed'
        : backlogCount > 0
          ? 'processing'
          : counters.graphSparse > 0
            ? 'graph_sparse'
            : counters.graphReady > 0
              ? 'graph_ready'
              : counters.readable > 0
                ? 'readable'
                : null,
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
  if (document.head?.latest_mutation_id) {
    const detailedMutation = relatedMutations.find(
      (mutation) => mutation.mutation.id === document.head?.latest_mutation_id,
    )
    if (detailedMutation) {
      return detailedMutation
    }
  }
  if (document.pipeline.latest_mutation) {
    return {
      mutation: document.pipeline.latest_mutation,
      items: [],
      job_id: document.pipeline.latest_job?.id ?? null,
      async_operation_id: document.pipeline.latest_job?.async_operation_id ?? null,
    }
  }
  return null
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
  const preparation = buildPreparationSummary(document)
  const documentStatus = documentStatusFromPreparationState(
    preparation,
    document.readiness,
    currentMutationStatus,
    currentJob?.queue_state ?? null,
  )
  const activityStatus = activityStatusFromCurrentState(
    documentStatus,
    currentJob,
    currentMutationStatus,
  )
  const stage = stageKeyFor(currentJob, currentMutationStatus)
  const activeRevision = document.active_revision
  const fileName = buildDocumentFileName(document, activeRevision, revisions)
  const fileType = buildDocumentFileType(document, activeRevision, revisions)
  const lastActivityAt = latestActivityAt(currentJob)
  const failureMessage = buildDocumentFailureMessage(currentMutation, currentJob, documentStatus)

  return {
    id: document.document.id,
    fileName,
    fileType,
    fileSizeBytes: activeRevision?.byte_size ?? null,
    fileSizeLabel: activeRevision ? formatFileSizeLabel(activeRevision.byte_size) : '—',
    uploadedAt: document.document.created_at,
    status: documentStatus,
    statusLabel: statusLabelFor(documentStatus, preparation),
    stage,
    stageLabel: stageLabelFor(currentJob, currentMutationStatus),
    progressPercent: progressPercentForState(preparation, documentStatus, activityStatus, stage),
    activityStatus,
    lastActivityAt,
    stalledReason: stalledReason(activityStatus, lastActivityAt),
    costAmount: null,
    costLabel: null,
    failureMessage,
    canRetry:
      documentStatus === 'failed' &&
      document.document.document_state === 'active' &&
      activeRevision !== null &&
      currentJob?.retryable === true,
    detailAvailable: document.document.document_state === 'active',
    preparation,
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

function mapChunksToPreview(chunks: RawChunkSummary[]): {
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

function mapDetailProcessingHistory(jobs: RawIngestJob[]): DocumentDetail['processingHistory'] {
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
  preparedSegments: PreparedSegmentRow[],
  technicalFacts: TechnicalFactRow[],
  relatedMutations: RawContentMutationDetailResponse[],
  relatedJobs: RawIngestJob[],
  latestMutation: RawContentMutationDetailResponse | null,
  libraryName: string,
  knowledgeRevision: RawKnowledgeRevisionRow | null,
): DocumentDetail {
  const activeRevision = document.active_revision
  const readableRevision = document.head?.readable_revision_id
    ? (revisions.find((revision) => revision.id === document.head?.readable_revision_id) ?? null)
    : activeRevision
  const currentMutation = selectCurrentMutation(document, relatedMutations)
  const currentJob = selectMutationJob(document, currentMutation, relatedJobs)
  const readiness = knowledgeRevision ?? document.readiness
  const preparation = buildPreparationSummary(document)
  const status = documentStatusFromPreparationState(
    preparation,
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
        warning: canonicalSummary.previewTruncated
          ? 'Preview truncated from canonical chunks.'
          : null,
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
  const resolvedMutation = currentMutation ?? latestMutation
  const resolvedJob = currentJob ?? latestJobForCanonical
  const errorMessage = buildDocumentFailureMessage(resolvedMutation, resolvedJob, status)
  const errorActionMessage = buildDocumentFailureActionMessage(resolvedMutation, resolvedJob)

  return {
    id: document.document.id,
    logicalDocumentId: document.document.id,
    contentSourceKind: activeRevision?.content_source_kind ?? null,
    sourceUri: activeRevision?.source_uri ?? null,
    webPageProvenance: buildWebPageProvenance(
      document.web_page_provenance ?? document.webPageProvenance ?? null,
    ),
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
    progressPercent: progressPercentForState(preparation, status, activityStatus, null),
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
    requestedBy: resolvedMutation?.mutation.requested_by_principal_id ?? null,
    errorMessage,
    errorActionMessage,
    failureClass:
      currentJob?.queue_state === 'failed' || currentJob?.queue_state === 'canceled'
        ? 'ingest_failed'
        : null,
    operatorAction: (currentMutation ?? latestMutation)?.mutation.operation_kind ?? null,
    summary:
      canonicalSummaryPreview?.text ?? activeRevision?.title ?? document.document.external_key,
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
    preparation,
    preparedSegments,
    technicalFacts,
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
  readinessSummary: LibraryReadinessSummary,
  graphCoverage: LibraryGraphCoverageSummary,
  summaryWarning: string | null = null,
): DocumentsSurfaceResponse {
  const rows = documents
    .map((document) => mapSurfaceRow(document, [], []))
    .sort((left, right) => compareIsoDates(left.uploadedAt, right.uploadedAt))

  const counters = normalizeReadinessCounters({
    ...(readinessSummary?.documentCountsByReadiness ?? {}),
  })
  const diagnostics = buildCollectionDiagnostics(rows, counters)
  const graphWarning =
    summaryWarning ??
    (rows.length === 0
      ? null
      : counters.failed > 0
        ? 'One or more canonical documents failed ingestion.'
        : counters.processing > 0
          ? 'Canonical ingestion is still in flight.'
          : counters.readable > 0 || graphCoverage.graphSparseDocumentCount > 0
            ? 'Some canonical documents are readable but graph coverage is still sparse.'
            : null)

  return {
    acceptedFormats: DEFAULT_ACCEPTED_FORMATS,
    maxSizeMb: DEFAULT_UPLOAD_LIMIT_MB,
    graphStatus:
      rows.length === 0
        ? 'empty'
        : summaryWarning
          ? 'partial'
          : counters.processing > 0
            ? 'building'
            : counters.failed > 0 && graphCoverage.graphReadyDocumentCount === 0
              ? 'failed'
              : counters.readable > 0 || graphCoverage.graphSparseDocumentCount > 0
                ? 'partial'
                : 'ready',
    graphWarning,
    rebuildBacklogCount: diagnostics.activeBacklogCount,
    readinessSummary: {
      ...readinessSummary,
      documentCountsByReadiness: counters,
    },
    graphCoverage,
    counters,
    filters: {
      statuses: Array.from(new Set(rows.map((row) => row.status))).sort(),
      fileTypes: Array.from(new Set(rows.map((row) => row.fileType))).sort(),
    },
    accounting: buildCollectionAccounting(counters),
    diagnostics,
    workspace: buildWorkspaceSummary(rows, counters, diagnostics),
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

export function normalizeDocumentUploadFailure(file: File, error: unknown): DocumentUploadFailure {
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

export async function fetchLibraryCostSummary(
  libraryId: string,
): Promise<LibraryCostSummary | null> {
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

export async function fetchLibraryKnowledgeSummary(
  libraryId?: string | null,
): Promise<LibraryKnowledgeSummaryResponse | null> {
  const resolvedLibraryId = resolveContextLibraryId(libraryId)
  if (!resolvedLibraryId) {
    return null
  }

  const response = await unwrap(
    apiHttp.get<RawKnowledgeLibrarySummaryResponse>(
      `/knowledge/libraries/${resolvedLibraryId}/summary`,
    ),
  )
  return mapLibraryKnowledgeSummary(resolvedLibraryId, response)
}

export async function fetchDocumentsSurface(): Promise<DocumentsSurfaceResponse> {
  const shellStore = useShellStore()
  const libraryId = shellStore.context?.activeLibrary.id ?? null
  const [documents, documentCosts, knowledgeSummaryProjection] = await Promise.all([
    fetchSurfaceDocumentsForLibrary(libraryId),
    libraryId ? fetchLibraryDocumentCosts(libraryId) : Promise.resolve([]),
    resolveLibraryKnowledgeSummaryProjection(libraryId),
  ])

  const summary =
    knowledgeSummaryProjection.summary ?? buildEmptyLibraryKnowledgeSummary(libraryId ?? '')
  const surface = buildSurfaceResponse(
    documents,
    summary.readinessSummary,
    summary.graphCoverage,
    knowledgeSummaryProjection.warning,
  )

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
  const knowledgeSummaryProjection = await resolveLibraryKnowledgeSummaryProjection(libraryId)
  return normalizeReadinessCounters({
    ...(knowledgeSummaryProjection.summary?.readinessSummary.documentCountsByReadiness ?? {}),
  })
}

export async function fetchDocumentDetail(id: string): Promise<DocumentDetail> {
  const shellStore = useShellStore()
  const document = normalizeContentDocumentDetailResponse(
    await unwrap(apiHttp.get<RawContentDocumentDetailResponse>(`/content/documents/${id}`)),
  )
  const [
    revisions,
    chunks,
    preparedSegmentsPage,
    technicalFactsPage,
    knowledgeDetail,
    documentCosts,
    latestMutation,
  ] = await Promise.all([
    unwrap(apiHttp.get<RawContentRevision[]>(`/content/documents/${id}/revisions`)),
    unwrap(
      apiHttp.get<RawChunkSummary[]>('/chunks', {
        params: { documentId: id },
      }),
    ),
    unwrap(
      apiHttp.get<RawPreparedSegmentsPageResponse>(`/content/documents/${id}/prepared-segments`, {
        params: { offset: 0, limit: 500 },
      }),
    ),
    unwrap(
      apiHttp.get<RawTechnicalFactsPageResponse>(`/content/documents/${id}/technical-facts`, {
        params: { offset: 0, limit: 500 },
      }),
    ),
    fetchKnowledgeDocumentDetail(document.document.library_id, id),
    fetchLibraryDocumentCosts(document.document.library_id),
    document.head?.latest_mutation_id
      ? unwrap(
          apiHttp.get<RawContentMutationDetailResponseWire>(
            `/content/mutations/${document.head.latest_mutation_id}`,
          ),
        ).then(normalizeContentMutationDetailResponse)
      : Promise.resolve(null),
  ])
  const preparedSegments = mapPreparedSegments(preparedSegmentsPage.items)
  const technicalFacts = mapTechnicalFacts(technicalFactsPage.items, preparedSegments)
  const relatedMutations: RawContentMutationDetailResponse[] = latestMutation
    ? [latestMutation]
    : []
  const relatedJobs: RawIngestJob[] = []
  const effectiveKnowledgeRevision = document.readiness?.revision_id
    ? (knowledgeDetail.revisions.find(
        (revision) => revision.revisionId === document.readiness?.revision_id,
      ) ?? knowledgeDetail.latestRevision)
    : knowledgeDetail.latestRevision
  const revisionsById = new Map(revisions.map((revision) => [revision.id, revision] as const))
  const readableRevision = document.head?.readable_revision_id
    ? (revisionsById.get(document.head.readable_revision_id) ?? null)
    : document.active_revision
  const detail = mapDocumentDetail(
    document,
    revisions,
    chunks,
    preparedSegments,
    technicalFacts,
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
    apiHttp.post<RawContentMutationDetailResponse>(`/content/documents/${id}/reprocess`, {}),
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
    statusLabel: statusLabelFor(detail.status, detail.preparation),
    stage: detail.stage,
    stageLabel:
      detail.status === 'processing' || detail.status === 'queued'
        ? documentStageLabel(detail.stage)
        : null,
    progressPercent: detail.progressPercent,
    activityStatus: detail.activityStatus,
    lastActivityAt: detail.lastActivityAt,
    stalledReason: detail.stalledReason,
    costAmount:
      detail.totalEstimatedCost && detail.totalEstimatedCost > 0 ? detail.totalEstimatedCost : null,
    costLabel:
      detail.totalEstimatedCost && detail.totalEstimatedCost > 0
        ? formatCostLabel(detail.totalEstimatedCost)
        : null,
    failureMessage: detail.errorMessage,
    canRetry: detail.canRetry,
    detailAvailable: detail.detailAvailable,
    preparation: detail.preparation,
  }
}

function buildMutationAcceptedResponse(
  response: RawContentMutationDetailResponseWire | RawContentMutationDetailResponse,
): DocumentMutationAccepted {
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

export async function createWebIngestRun(
  input: CreateWebIngestRunInput,
): Promise<WebIngestRunReceipt> {
  return mapWebIngestRunReceipt(
    await unwrap(apiHttp.post<RawWebIngestRunReceipt>('/content/web-runs', input)),
  )
}

export async function fetchWebIngestRuns(libraryId: string): Promise<WebIngestRunSummary[]> {
  const runs = await unwrap(
    apiHttp.get<RawWebIngestRunSummary[]>('/content/web-runs', {
      params: { libraryId },
    }),
  )
  return runs.map(mapWebIngestRunSummary)
}

export async function fetchWebIngestRun(runId: string): Promise<WebIngestRun> {
  return mapWebIngestRun(await unwrap(apiHttp.get<RawWebIngestRun>(`/content/web-runs/${runId}`)))
}

export async function fetchWebIngestRunPages(runId: string): Promise<WebDiscoveredPage[]> {
  const pages = await unwrap(
    apiHttp.get<RawWebDiscoveredPage[]>(`/content/web-runs/${runId}/pages`),
  )
  return pages.map(mapWebDiscoveredPage)
}

export async function cancelWebIngestRun(runId: string): Promise<WebIngestRunReceipt> {
  return mapWebIngestRunReceipt(
    await unwrap(apiHttp.post<RawWebIngestRunReceipt>(`/content/web-runs/${runId}/cancel`)),
  )
}
