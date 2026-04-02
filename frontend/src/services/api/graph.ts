import type {
  GraphCanonicalSummary,
  GraphConvergenceStatus,
  GraphDiagnostics,
  GraphEdge,
  GraphEvidence,
  GraphLegendItem,
  GraphNode,
  GraphNodeDetail,
  GraphNodeType,
  GraphSearchHit,
  GraphStatus,
  GraphSurfaceResponse,
} from 'src/models/ui/graph'
import { resolveDefaultGraphLayoutMode } from 'src/models/ui/graph'
import type { DashboardAttentionItem } from 'src/models/ui/dashboard'
import type {
  DocumentsSurfaceResponse,
  LibraryGraphCoverageSummary,
  LibraryReadinessSummary,
} from 'src/models/ui/documents'
import { i18n } from 'src/lib/i18n'
import { useShellStore } from 'src/stores/shell'
import {
  buildEmptyLibraryKnowledgeSummary,
  resolveLibraryKnowledgeSummaryProjection,
} from './documents'
import { ApiClientError, apiHttp, unwrap } from './http'
import {
  normalizeWireNullableNumber,
  normalizeWireNullableString,
  normalizeWireNumber,
  normalizeWireString,
  normalizeWireStringArray,
  readWireValue,
  type WireRecord,
} from './wire'

interface RawKnowledgeDocumentRow {
  key: string
  arangoId?: string | null
  arangoRev?: string | null
  documentId: string
  workspaceId: string
  libraryId: string
  externalKey: string
  title: string | null
  documentState: string
  activeRevisionId: string | null
  readableRevisionId: string | null
  latestRevisionNo: number | null
  createdAt: string
  updatedAt: string
  deletedAt: string | null
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

interface RawKnowledgeChunkRow {
  key: string
  arangoId?: string | null
  arangoRev?: string | null
  chunkId: string
  workspaceId: string
  libraryId: string
  documentId: string
  revisionId: string
  chunkIndex: number
  contentText: string
  normalizedText: string
  spanStart: number | null
  spanEnd: number | null
  tokenCount: number | null
  sectionPath: string[]
  headingTrail: string[]
  chunkState: string
  textGeneration: number | null
  vectorGeneration: number | null
}

interface RawKnowledgeLibraryGenerationRow {
  key: string
  arangoId?: string | null
  arangoRev?: string | null
  generationId: string
  workspaceId: string
  libraryId: string
  activeTextGeneration: number
  activeVectorGeneration: number
  activeGraphGeneration: number
  degradedState: string
  updatedAt: string
}

interface RawKnowledgeEntityRow {
  key: string
  arangoId?: string | null
  arangoRev?: string | null
  entityId: string
  workspaceId: string
  libraryId: string
  canonicalLabel: string
  aliases: string[]
  entityType: string
  summary: string | null
  confidence: number | null
  supportCount: number
  freshnessGeneration: number
  entityState: string
  createdAt: string
  updatedAt: string
}

interface RawKnowledgeRelationRow {
  key: string
  arangoId?: string | null
  arangoRev?: string | null
  relationId: string
  workspaceId: string
  libraryId: string
  predicate: string
  normalizedAssertion: string
  confidence: number | null
  supportCount: number
  contradictionState: string
  freshnessGeneration: number
  relationState: string
  subjectEntityId?: string | null
  objectEntityId?: string | null
  createdAt: string
  updatedAt: string
}

interface RawKnowledgeEvidenceRow {
  key: string
  arangoId?: string | null
  arangoRev?: string | null
  evidenceId: string
  workspaceId: string
  libraryId: string
  documentId: string
  revisionId: string
  chunkId: string | null
  spanStart: number | null
  spanEnd: number | null
  excerpt: string
  supportKind: string
  extractionMethod: string
  confidence: number | null
  evidenceState: string
  freshnessGeneration: number
  createdAt: string
  updatedAt: string
}

interface RawKnowledgeDocumentDetailResponse {
  document: RawKnowledgeDocumentRow
  revisions: RawKnowledgeRevisionRow[]
  latestRevision: RawKnowledgeRevisionRow | null
  latestRevisionChunks: RawKnowledgeChunkRow[]
}

interface RawKnowledgeEntityDetailResponse {
  entity: RawKnowledgeEntityRow
  mentionEdges: {
    key: string
    entityId: string
    chunkId: string
    rank: number | null
    score: number | null
    inclusionReason: string | null
    createdAt: string
  }[]
  mentionedChunks: RawKnowledgeChunkRow[]
  supportingEvidenceEdges: {
    key: string
    evidenceId: string
    entityId: string
    rank: number | null
    score: number | null
    inclusionReason: string | null
    createdAt: string
  }[]
  supportingEvidence: RawKnowledgeEvidenceRow[]
}

interface RawKnowledgeRelationDetailResponse {
  relation: RawKnowledgeRelationRow
  supportingEvidenceEdges: {
    key: string
    evidenceId: string
    relationId: string
    rank: number | null
    score: number | null
    inclusionReason: string | null
    createdAt: string
  }[]
  supportingEvidence: RawKnowledgeEvidenceRow[]
}

interface RawKnowledgeDocumentGraphLinkRow {
  documentId: string
  targetNodeId: string
  targetNodeType: GraphNodeType
  relationType: string
  supportCount: number
}

interface RawKnowledgeGraphTopologyResponse {
  documents: RawKnowledgeDocumentRow[]
  entities: RawKnowledgeEntityRow[]
  relations: RawKnowledgeRelationRow[]
  documentLinks: RawKnowledgeDocumentGraphLinkRow[]
}

export interface GraphSurfaceHeartbeat {
  graphStatus: GraphStatus
  convergenceStatus: GraphConvergenceStatus | null
  graphGeneration: number
  graphGenerationState: string | null
  lastBuiltAt: string | null
  readinessSummary: LibraryReadinessSummary | null
  graphCoverage: LibraryGraphCoverageSummary | null
  warning: string | null
}

function emptyReadinessSummary(libraryId = ''): LibraryReadinessSummary {
  return buildEmptyLibraryKnowledgeSummary(libraryId).readinessSummary
}

function emptyGraphCoverage(libraryId = ''): LibraryGraphCoverageSummary {
  return buildEmptyLibraryKnowledgeSummary(libraryId).graphCoverage
}

function normalizeKnowledgeDocumentRow(row: WireRecord): RawKnowledgeDocumentRow {
  return {
    key: normalizeWireString(readWireValue(row, 'key', '_key')),
    arangoId: normalizeWireNullableString(readWireValue(row, 'arangoId', '_id')),
    arangoRev: normalizeWireNullableString(readWireValue(row, 'arangoRev', '_rev')),
    documentId: normalizeWireString(readWireValue(row, 'documentId', 'document_id')),
    workspaceId: normalizeWireString(readWireValue(row, 'workspaceId', 'workspace_id')),
    libraryId: normalizeWireString(readWireValue(row, 'libraryId', 'library_id')),
    externalKey: normalizeWireString(readWireValue(row, 'externalKey', 'external_key')),
    title: normalizeWireNullableString(readWireValue(row, 'title')),
    documentState: normalizeWireString(readWireValue(row, 'documentState', 'document_state')),
    activeRevisionId: normalizeWireNullableString(
      readWireValue(row, 'activeRevisionId', 'active_revision_id'),
    ),
    readableRevisionId: normalizeWireNullableString(
      readWireValue(row, 'readableRevisionId', 'readable_revision_id'),
    ),
    latestRevisionNo: normalizeWireNullableNumber(
      readWireValue(row, 'latestRevisionNo', 'latest_revision_no'),
    ),
    createdAt: normalizeWireString(readWireValue(row, 'createdAt', 'created_at')),
    updatedAt: normalizeWireString(readWireValue(row, 'updatedAt', 'updated_at')),
    deletedAt: normalizeWireNullableString(readWireValue(row, 'deletedAt', 'deleted_at')),
  }
}

function normalizeKnowledgeRevisionRow(row: WireRecord): RawKnowledgeRevisionRow {
  return {
    key: normalizeWireString(readWireValue(row, 'key', '_key')),
    arangoId: normalizeWireNullableString(readWireValue(row, 'arangoId', '_id')),
    arangoRev: normalizeWireNullableString(readWireValue(row, 'arangoRev', '_rev')),
    revisionId: normalizeWireString(readWireValue(row, 'revisionId', 'revision_id')),
    workspaceId: normalizeWireString(readWireValue(row, 'workspaceId', 'workspace_id')),
    libraryId: normalizeWireString(readWireValue(row, 'libraryId', 'library_id')),
    documentId: normalizeWireString(readWireValue(row, 'documentId', 'document_id')),
    revisionNumber: normalizeWireNumber(readWireValue(row, 'revisionNumber', 'revision_number')),
    revisionState: normalizeWireString(readWireValue(row, 'revisionState', 'revision_state')),
    revisionKind: normalizeWireString(readWireValue(row, 'revisionKind', 'revision_kind')),
    storageRef: normalizeWireNullableString(readWireValue(row, 'storageRef', 'storage_ref')),
    mimeType: normalizeWireString(readWireValue(row, 'mimeType', 'mime_type')),
    checksum: normalizeWireString(readWireValue(row, 'checksum')),
    title: normalizeWireNullableString(readWireValue(row, 'title')),
    byteSize: normalizeWireNumber(readWireValue(row, 'byteSize', 'byte_size')),
    normalizedText: normalizeWireNullableString(
      readWireValue(row, 'normalizedText', 'normalized_text'),
    ),
    textChecksum: normalizeWireNullableString(readWireValue(row, 'textChecksum', 'text_checksum')),
    textState: normalizeWireString(readWireValue(row, 'textState', 'text_state')),
    vectorState: normalizeWireString(readWireValue(row, 'vectorState', 'vector_state')),
    graphState: normalizeWireString(readWireValue(row, 'graphState', 'graph_state')),
    textReadableAt: normalizeWireNullableString(
      readWireValue(row, 'textReadableAt', 'text_readable_at'),
    ),
    vectorReadyAt: normalizeWireNullableString(
      readWireValue(row, 'vectorReadyAt', 'vector_ready_at'),
    ),
    graphReadyAt: normalizeWireNullableString(readWireValue(row, 'graphReadyAt', 'graph_ready_at')),
    supersededByRevisionId: normalizeWireNullableString(
      readWireValue(row, 'supersededByRevisionId', 'superseded_by_revision_id'),
    ),
    createdAt: normalizeWireString(readWireValue(row, 'createdAt', 'created_at')),
  }
}

function normalizeKnowledgeChunkRow(row: WireRecord): RawKnowledgeChunkRow {
  return {
    key: normalizeWireString(readWireValue(row, 'key', '_key')),
    arangoId: normalizeWireNullableString(readWireValue(row, 'arangoId', '_id')),
    arangoRev: normalizeWireNullableString(readWireValue(row, 'arangoRev', '_rev')),
    chunkId: normalizeWireString(readWireValue(row, 'chunkId', 'chunk_id')),
    workspaceId: normalizeWireString(readWireValue(row, 'workspaceId', 'workspace_id')),
    libraryId: normalizeWireString(readWireValue(row, 'libraryId', 'library_id')),
    documentId: normalizeWireString(readWireValue(row, 'documentId', 'document_id')),
    revisionId: normalizeWireString(readWireValue(row, 'revisionId', 'revision_id')),
    chunkIndex: normalizeWireNumber(readWireValue(row, 'chunkIndex', 'chunk_index')),
    contentText: normalizeWireString(readWireValue(row, 'contentText', 'content_text')),
    normalizedText: normalizeWireString(readWireValue(row, 'normalizedText', 'normalized_text')),
    spanStart: normalizeWireNullableNumber(readWireValue(row, 'spanStart', 'span_start')),
    spanEnd: normalizeWireNullableNumber(readWireValue(row, 'spanEnd', 'span_end')),
    tokenCount: normalizeWireNullableNumber(readWireValue(row, 'tokenCount', 'token_count')),
    sectionPath: normalizeWireStringArray(readWireValue(row, 'sectionPath', 'section_path')),
    headingTrail: normalizeWireStringArray(readWireValue(row, 'headingTrail', 'heading_trail')),
    chunkState: normalizeWireString(readWireValue(row, 'chunkState', 'chunk_state')),
    textGeneration: normalizeWireNullableNumber(
      readWireValue(row, 'textGeneration', 'text_generation'),
    ),
    vectorGeneration: normalizeWireNullableNumber(
      readWireValue(row, 'vectorGeneration', 'vector_generation'),
    ),
  }
}

function normalizeKnowledgeGenerationRow(row: WireRecord): RawKnowledgeLibraryGenerationRow {
  const degradedState = normalizeWireString(
    readWireValue(row, 'degradedState', 'degraded_state', 'generationState', 'generation_state'),
  )
  const updatedAt = normalizeWireString(
    readWireValue(
      row,
      'updatedAt',
      'updated_at',
      'completedAt',
      'completed_at',
      'createdAt',
      'created_at',
    ),
  )

  return {
    key: normalizeWireString(readWireValue(row, 'key', '_key')),
    arangoId: normalizeWireNullableString(readWireValue(row, 'arangoId', '_id')),
    arangoRev: normalizeWireNullableString(readWireValue(row, 'arangoRev', '_rev')),
    generationId: normalizeWireString(readWireValue(row, 'generationId', 'generation_id', 'id')),
    workspaceId: normalizeWireString(readWireValue(row, 'workspaceId', 'workspace_id')),
    libraryId: normalizeWireString(readWireValue(row, 'libraryId', 'library_id')),
    activeTextGeneration: normalizeWireNumber(
      readWireValue(row, 'activeTextGeneration', 'active_text_generation'),
      degradedState === 'text_readable' ? 1 : 0,
    ),
    activeVectorGeneration: normalizeWireNumber(
      readWireValue(row, 'activeVectorGeneration', 'active_vector_generation'),
      degradedState === 'vector_ready' ? 1 : 0,
    ),
    activeGraphGeneration: normalizeWireNumber(
      readWireValue(row, 'activeGraphGeneration', 'active_graph_generation'),
      degradedState === 'graph_ready' || degradedState === 'ready' ? 1 : 0,
    ),
    degradedState,
    updatedAt,
  }
}

function normalizeKnowledgeEntityRow(row: WireRecord): RawKnowledgeEntityRow {
  return {
    key: normalizeWireString(readWireValue(row, 'key', '_key')),
    arangoId: normalizeWireNullableString(readWireValue(row, 'arangoId', '_id')),
    arangoRev: normalizeWireNullableString(readWireValue(row, 'arangoRev', '_rev')),
    entityId: normalizeWireString(readWireValue(row, 'entityId', 'entity_id')),
    workspaceId: normalizeWireString(readWireValue(row, 'workspaceId', 'workspace_id')),
    libraryId: normalizeWireString(readWireValue(row, 'libraryId', 'library_id')),
    canonicalLabel: normalizeWireString(readWireValue(row, 'canonicalLabel', 'canonical_label')),
    aliases: normalizeWireStringArray(readWireValue(row, 'aliases')),
    entityType: normalizeWireString(readWireValue(row, 'entityType', 'entity_type')),
    summary: normalizeWireNullableString(readWireValue(row, 'summary')),
    confidence: normalizeWireNullableNumber(readWireValue(row, 'confidence')),
    supportCount: normalizeWireNumber(readWireValue(row, 'supportCount', 'support_count')),
    freshnessGeneration: normalizeWireNumber(
      readWireValue(row, 'freshnessGeneration', 'freshness_generation'),
    ),
    entityState: normalizeWireString(readWireValue(row, 'entityState', 'entity_state')),
    createdAt: normalizeWireString(readWireValue(row, 'createdAt', 'created_at')),
    updatedAt: normalizeWireString(readWireValue(row, 'updatedAt', 'updated_at')),
  }
}

function normalizeKnowledgeRelationRow(row: WireRecord): RawKnowledgeRelationRow {
  return {
    key: normalizeWireString(readWireValue(row, 'key', '_key')),
    arangoId: normalizeWireNullableString(readWireValue(row, 'arangoId', '_id')),
    arangoRev: normalizeWireNullableString(readWireValue(row, 'arangoRev', '_rev')),
    relationId: normalizeWireString(readWireValue(row, 'relationId', 'relation_id')),
    workspaceId: normalizeWireString(readWireValue(row, 'workspaceId', 'workspace_id')),
    libraryId: normalizeWireString(readWireValue(row, 'libraryId', 'library_id')),
    predicate: normalizeWireString(readWireValue(row, 'predicate')),
    normalizedAssertion: normalizeWireString(
      readWireValue(row, 'normalizedAssertion', 'normalized_assertion'),
    ),
    confidence: normalizeWireNullableNumber(readWireValue(row, 'confidence')),
    supportCount: normalizeWireNumber(readWireValue(row, 'supportCount', 'support_count')),
    contradictionState: normalizeWireString(
      readWireValue(row, 'contradictionState', 'contradiction_state'),
    ),
    freshnessGeneration: normalizeWireNumber(
      readWireValue(row, 'freshnessGeneration', 'freshness_generation'),
    ),
    relationState: normalizeWireString(readWireValue(row, 'relationState', 'relation_state')),
    subjectEntityId: normalizeWireNullableString(
      readWireValue(row, 'subjectEntityId', 'subject_entity_id'),
    ),
    objectEntityId: normalizeWireNullableString(
      readWireValue(row, 'objectEntityId', 'object_entity_id'),
    ),
    createdAt: normalizeWireString(readWireValue(row, 'createdAt', 'created_at')),
    updatedAt: normalizeWireString(readWireValue(row, 'updatedAt', 'updated_at')),
  }
}

function normalizeKnowledgeEvidenceRow(row: WireRecord): RawKnowledgeEvidenceRow {
  return {
    key: normalizeWireString(readWireValue(row, 'key', '_key')),
    arangoId: normalizeWireNullableString(readWireValue(row, 'arangoId', '_id')),
    arangoRev: normalizeWireNullableString(readWireValue(row, 'arangoRev', '_rev')),
    evidenceId: normalizeWireString(readWireValue(row, 'evidenceId', 'evidence_id')),
    workspaceId: normalizeWireString(readWireValue(row, 'workspaceId', 'workspace_id')),
    libraryId: normalizeWireString(readWireValue(row, 'libraryId', 'library_id')),
    documentId: normalizeWireString(readWireValue(row, 'documentId', 'document_id')),
    revisionId: normalizeWireString(readWireValue(row, 'revisionId', 'revision_id')),
    chunkId: normalizeWireNullableString(readWireValue(row, 'chunkId', 'chunk_id')),
    spanStart: normalizeWireNullableNumber(readWireValue(row, 'spanStart', 'span_start')),
    spanEnd: normalizeWireNullableNumber(readWireValue(row, 'spanEnd', 'span_end')),
    excerpt: normalizeWireString(readWireValue(row, 'excerpt')),
    supportKind: normalizeWireString(readWireValue(row, 'supportKind', 'support_kind')),
    extractionMethod: normalizeWireString(
      readWireValue(row, 'extractionMethod', 'extraction_method'),
    ),
    confidence: normalizeWireNullableNumber(readWireValue(row, 'confidence')),
    evidenceState: normalizeWireString(readWireValue(row, 'evidenceState', 'evidence_state')),
    freshnessGeneration: normalizeWireNumber(
      readWireValue(row, 'freshnessGeneration', 'freshness_generation'),
    ),
    createdAt: normalizeWireString(readWireValue(row, 'createdAt', 'created_at')),
    updatedAt: normalizeWireString(readWireValue(row, 'updatedAt', 'updated_at')),
  }
}

function normalizeKnowledgeDocumentGraphLinkRow(row: WireRecord): RawKnowledgeDocumentGraphLinkRow {
  return {
    documentId: normalizeWireString(readWireValue(row, 'documentId', 'document_id')),
    targetNodeId: normalizeWireString(readWireValue(row, 'targetNodeId', 'target_node_id')),
    targetNodeType: resolveNodeType(
      normalizeWireString(readWireValue(row, 'targetNodeType', 'target_node_type'), 'entity'),
    ),
    relationType: normalizeWireString(
      readWireValue(row, 'relationType', 'relation_type'),
      'supports',
    ),
    supportCount: normalizeWireNumber(readWireValue(row, 'supportCount', 'support_count')),
  }
}

function resolveNodeType(kind: string): GraphNodeType {
  const normalized = kind.trim().toLowerCase()
  if (normalized === 'document') {
    return 'document'
  }
  if (normalized === 'relation') {
    return 'topic'
  }
  if (normalized === 'topic' || normalized === 'concept' || normalized === 'theme') {
    return 'topic'
  }
  return 'entity'
}

function resolveActiveLibraryId(): string | null {
  return useShellStore().context?.activeLibrary.id ?? null
}

function buildLegend(nodes: GraphNode[], edgeCount: number): GraphLegendItem[] {
  const kinds = new Set(nodes.map((node) => node.nodeType))
  const items: GraphLegendItem[] = []

  if (kinds.has('document')) {
    items.push({ key: 'document', label: 'Document' })
  }
  if (kinds.has('entity')) {
    items.push({ key: 'entity', label: 'Entity' })
  }
  if (kinds.has('topic')) {
    items.push({ key: 'topic', label: 'Relation' })
  }
  if (edgeCount > 0) {
    items.push({ key: 'relation', label: 'Relation links' })
  }

  return items
}

function buildEmptySurface(): GraphSurfaceResponse {
  return {
    loading: false,
    error: null,
    canvasMode: 'empty',
    graphStatus: 'empty',
    convergenceStatus: null,
    graphGeneration: 0,
    graphGenerationState: null,
    nodeCount: 0,
    relationCount: 0,
    edgeCount: 0,
    hiddenNodeCount: 0,
    filteredArtifactCount: 0,
    lastBuiltAt: null,
    readinessSummary: emptyReadinessSummary(),
    graphCoverage: emptyGraphCoverage(),
    overlay: {
      searchQuery: '',
      searchHits: [],
      nodeTypeFilter: '',
      activeLayout: resolveDefaultGraphLayoutMode(0, 0),
      showFilteredArtifacts: false,
      filteredArtifactCount: 0,
      nodeCount: 0,
      edgeCount: 0,
      showLegend: false,
      showFilters: false,
      zoomLevel: 1,
    },
    inspector: {
      focusedNodeId: null,
      loading: false,
      error: null,
      detail: null,
    },
    warning: null,
    nodes: [],
    edges: [],
    legend: [],
  }
}

function graphGenerationOf(generation: RawKnowledgeLibraryGenerationRow | null): number {
  if (!generation) {
    return 0
  }
  const parsed = Date.parse(generation.updatedAt)
  return Number.isFinite(parsed) ? parsed : 0
}

function mapGraphStatus(
  generation: RawKnowledgeLibraryGenerationRow | null,
  nodeCount: number,
  relationCount: number,
): GraphStatus {
  if (!generation) {
    return nodeCount > 0 || relationCount > 0 ? 'partial' : 'empty'
  }

  switch (generation.degradedState.trim().toLowerCase()) {
    case 'graph_ready':
    case 'ready':
      return nodeCount > 0 || relationCount > 0 ? 'ready' : 'empty'
    case 'vector_ready':
    case 'text_readable':
    case 'text_only':
      return nodeCount > 0 || relationCount > 0 ? 'partial' : 'building'
    case 'accepted':
      return nodeCount > 0 || relationCount > 0 ? 'partial' : 'building'
    case 'rebuilding':
      return 'rebuilding'
    case 'stale':
      return 'stale'
    case 'failed':
      return 'failed'
    default:
      return nodeCount > 0 || relationCount > 0 ? 'partial' : 'building'
  }
}

function mapConvergenceStatus(
  graphStatus: GraphStatus,
  generationState: string | null,
): GraphConvergenceStatus | null {
  if (graphStatus === 'ready') {
    return 'current'
  }
  if (graphStatus === 'partial' || graphStatus === 'rebuilding' || generationState === 'accepted') {
    return 'partial'
  }
  if (graphStatus === 'failed' || graphStatus === 'stale') {
    return 'degraded'
  }
  return null
}

function projectionWarning(
  graphStatus: GraphStatus,
  generationState: string | null,
): string | null {
  if (graphStatus === 'failed') {
    return 'The canonical Arango knowledge graph generation failed.'
  }
  if (graphStatus === 'building' || graphStatus === 'partial') {
    return generationState
      ? `The canonical Arango knowledge generation is ${generationState.replace(/_/g, ' ')}.`
      : 'The canonical Arango knowledge graph is still building.'
  }
  if (graphStatus === 'rebuilding') {
    return 'The canonical Arango knowledge graph is rebuilding after recent changes.'
  }
  if (graphStatus === 'stale') {
    return 'The canonical Arango knowledge graph is stale.'
  }
  return null
}

function buildSurface(
  generation: RawKnowledgeLibraryGenerationRow | null,
  rawNodes: GraphNode[],
  rawEdges: GraphEdge[] = [],
  hiddenNodeCount = 0,
  readinessSummary: LibraryReadinessSummary | null = emptyReadinessSummary(),
  graphCoverage: LibraryGraphCoverageSummary | null = emptyGraphCoverage(),
  warningOverride: string | null = null,
): GraphSurfaceResponse {
  const relationNodeCount = rawNodes.filter((node) => node.nodeType === 'topic').length
  const graphStatus = mapGraphStatus(generation, rawNodes.length, relationNodeCount)
  const generationState = generation?.degradedState ?? null
  const filteredArtifactCount =
    rawNodes.filter((node) => node.filteredArtifact).length +
    rawEdges.filter((edge) => edge.filteredArtifact).length
  const edgeCount = rawEdges.length
  const canvasMode =
    graphStatus === 'failed'
      ? 'error'
      : graphStatus === 'empty' && rawNodes.length === 0
        ? 'empty'
        : (graphStatus === 'building' || graphStatus === 'rebuilding') && rawNodes.length === 0
          ? 'building'
          : relationNodeCount === 0 &&
              rawNodes.length > 0 &&
              rawNodes.every((node) => node.nodeType === 'document') &&
              graphStatus !== 'building' &&
              graphStatus !== 'rebuilding' &&
              graphStatus !== 'empty'
            ? 'sparse'
            : 'ready'

  return {
    loading: false,
    error: null,
    canvasMode,
    graphStatus,
    convergenceStatus: mapConvergenceStatus(graphStatus, generationState),
    graphGeneration: graphGenerationOf(generation),
    graphGenerationState: generationState,
    nodeCount: rawNodes.length,
    relationCount: relationNodeCount,
    edgeCount,
    hiddenNodeCount,
    filteredArtifactCount,
    lastBuiltAt: generation?.updatedAt ?? null,
    readinessSummary,
    graphCoverage,
    overlay: {
      searchQuery: '',
      searchHits: [],
      nodeTypeFilter: '',
      activeLayout: resolveDefaultGraphLayoutMode(rawNodes.length, edgeCount),
      showFilteredArtifacts: false,
      filteredArtifactCount,
      nodeCount: rawNodes.length,
      edgeCount,
      showLegend: false,
      showFilters: false,
      zoomLevel: 1,
    },
    inspector: {
      focusedNodeId: null,
      loading: false,
      error: null,
      detail: null,
    },
    warning: warningOverride ?? projectionWarning(graphStatus, generationState),
    nodes: rawNodes,
    edges: rawEdges,
    legend: buildLegend(rawNodes, rawEdges.length),
  }
}

function findNodeById(nodes: GraphNode[], nodeId: string): GraphNode | null {
  return (
    nodes.find((node) => node.id === nodeId) ??
    nodes.find((node) => node.canonicalKey === `document:${nodeId}`) ??
    nodes.find((node) => node.canonicalKey === `entity:${nodeId}`) ??
    nodes.find((node) => node.canonicalKey === `relation:${nodeId}`) ??
    null
  )
}

function findDocumentNode(nodes: GraphNode[], documentId: string): GraphNode | null {
  return (
    nodes.find((node) => node.id === documentId) ??
    nodes.find((node) => node.canonicalKey === `document:${documentId}`) ??
    null
  )
}

function findEntityNode(nodes: GraphNode[], entityId: string): GraphNode | null {
  return (
    nodes.find((node) => node.id === entityId) ??
    nodes.find((node) => node.canonicalKey === `entity:${entityId}`) ??
    null
  )
}

function looksOpaqueIdentifier(value: string): boolean {
  const trimmed = value.trim()
  return (
    /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(trimmed) ||
    /^[0-9a-f]{4,}-[0-9a-f-]{20,}$/i.test(trimmed)
  )
}

function compactOpaqueIdentifier(value: string): string {
  const trimmed = value.trim()
  if (trimmed.length <= 18) {
    return trimmed
  }
  return `${trimmed.slice(0, 8)}…${trimmed.slice(-6)}`
}

function documentDisplayLabel(row: RawKnowledgeDocumentRow): string {
  const title = row.title?.trim() ?? ''
  if (title) {
    return title
  }
  const externalKey = row.externalKey.trim()
  if (!externalKey) {
    return compactOpaqueIdentifier(row.documentId)
  }
  if (externalKey === row.documentId || looksOpaqueIdentifier(externalKey)) {
    return compactOpaqueIdentifier(row.documentId)
  }
  return externalKey
}

function humanizeGraphLabelToken(value: string): string {
  const normalized = value
    .trim()
    .replace(/^entity:/i, '')
    .replace(/^relation:/i, '')
    .replace(/:+/g, ' ')
    .replace(/[_-]+/g, ' ')
    .replace(/\s+/g, ' ')
    .trim()

  if (!normalized) {
    return ''
  }

  return normalized
    .split(' ')
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1).toLowerCase())
    .join(' ')
}

function looksMachineEntityLabel(value: string): boolean {
  const trimmed = value.trim()
  if (!trimmed) {
    return false
  }

  return (
    /^entity:/i.test(trimmed) ||
    /^https?:/i.test(trimmed) ||
    /^https?_/i.test(trimmed) ||
    /^www[._-]/i.test(trimmed) ||
    trimmed.includes('/') ||
    trimmed.includes('_')
  )
}

function humanizeEntityLabel(value: string): string {
  const tokens = value
    .trim()
    .replace(/^entity:/i, '')
    .split(/[\s:/._-]+/)
    .map((part) => part.trim())
    .filter(Boolean)
    .filter((part) => !['entity', 'http', 'https', 'www'].includes(part.toLowerCase()))

  if (!tokens.length) {
    return ''
  }

  const displayTokens =
    tokens.length > 6 ? [...tokens.slice(0, 3), '...', ...tokens.slice(-2)] : tokens

  return displayTokens
    .map((part) =>
      part === '...' ? part : part.charAt(0).toUpperCase() + part.slice(1).toLowerCase(),
    )
    .join(' ')
}

function entityDisplayLabel(row: RawKnowledgeEntityRow): string {
  const canonicalLabel = row.canonicalLabel.trim()
  if (canonicalLabel && canonicalLabel !== 'entity:' && canonicalLabel !== 'entity') {
    if (!looksMachineEntityLabel(canonicalLabel)) {
      return canonicalLabel
    }

    const humanizedCanonicalLabel = humanizeEntityLabel(canonicalLabel)
    if (humanizedCanonicalLabel) {
      return humanizedCanonicalLabel
    }
  }

  return humanizeGraphLabelToken(row.entityType) || compactOpaqueIdentifier(row.entityId)
}

function looksMachineRelationLabel(value: string): boolean {
  const trimmed = value.trim()
  return (
    !trimmed || trimmed.includes('--') || /^entity:/i.test(trimmed) || /^relation:/i.test(trimmed)
  )
}

function relationDisplayLabel(row: RawKnowledgeRelationRow): string {
  const assertion = row.normalizedAssertion.trim()
  if (assertion && !looksMachineRelationLabel(assertion)) {
    return assertion
  }

  return (
    humanizeGraphLabelToken(row.predicate) ||
    humanizeGraphLabelToken(assertion) ||
    compactOpaqueIdentifier(row.relationId)
  )
}

function mapDocumentRow(row: RawKnowledgeDocumentRow): GraphNode {
  return {
    id: row.documentId,
    canonicalKey: `document:${row.documentId}`,
    label: documentDisplayLabel(row),
    nodeType: 'document',
    secondaryLabel: row.documentState,
    supportCount: row.latestRevisionNo ?? 1,
    filteredArtifact: false,
  }
}

function mapEntityRow(row: RawKnowledgeEntityRow): GraphNode {
  return {
    id: row.entityId,
    canonicalKey: `entity:${row.entityId}`,
    label: entityDisplayLabel(row),
    nodeType: 'entity',
    secondaryLabel: row.entityType,
    supportCount: row.supportCount,
    filteredArtifact: false,
  }
}

function mapRelationRow(row: RawKnowledgeRelationRow): GraphNode {
  return {
    id: row.relationId,
    canonicalKey: `relation:${row.relationId}`,
    label: relationDisplayLabel(row),
    nodeType: 'topic',
    secondaryLabel: row.predicate,
    supportCount: row.supportCount,
    filteredArtifact: false,
  }
}

function dedupeSearchHits(items: GraphSearchHit[]): GraphSearchHit[] {
  const seen = new Set<string>()
  return items.filter((item) => {
    if (seen.has(item.id)) {
      return false
    }
    seen.add(item.id)
    return true
  })
}

function buildGraphEdges(
  relations: RawKnowledgeRelationRow[],
  documentLinks: RawKnowledgeDocumentGraphLinkRow[],
  nodes: GraphNode[],
): GraphEdge[] {
  const nodeIds = new Set(nodes.map((node) => node.id))
  const relationEdges = relations.flatMap((relation) => {
    const subjectEntityId = relation.subjectEntityId ?? null
    const objectEntityId = relation.objectEntityId ?? null
    if (
      !subjectEntityId ||
      !objectEntityId ||
      !nodeIds.has(relation.relationId) ||
      !nodeIds.has(subjectEntityId) ||
      !nodeIds.has(objectEntityId)
    ) {
      return []
    }

    return [
      {
        id: `${relation.relationId}:subject`,
        canonicalKey: `relation-edge:${relation.relationId}:subject`,
        source: subjectEntityId,
        target: relation.relationId,
        relationType: 'subject',
        supportCount: relation.supportCount,
        filteredArtifact: false,
      },
      {
        id: `${relation.relationId}:object`,
        canonicalKey: `relation-edge:${relation.relationId}:object`,
        source: relation.relationId,
        target: objectEntityId,
        relationType: relation.predicate || 'object',
        supportCount: relation.supportCount,
        filteredArtifact: false,
      },
    ]
  })

  const documentLinkEdges = documentLinks.flatMap((link) => {
    if (
      !link.documentId ||
      !link.targetNodeId ||
      !nodeIds.has(link.documentId) ||
      !nodeIds.has(link.targetNodeId)
    ) {
      return []
    }

    return [
      {
        id: `${link.documentId}:${link.relationType}:${link.targetNodeId}`,
        canonicalKey: `document-edge:${link.documentId}:${link.relationType}:${link.targetNodeId}`,
        source: link.documentId,
        target: link.targetNodeId,
        relationType: link.relationType,
        supportCount: Math.max(1, link.supportCount),
        filteredArtifact: false,
      },
    ]
  })

  return [...documentLinkEdges, ...relationEdges]
}

function mapSearchNodeType(node: GraphNode): GraphNodeType {
  return node.nodeType
}

function resolveSearchPreview(node: GraphNode, preview: string | null): string | null {
  const trimmedPreview = preview?.trim() ?? ''
  if (trimmedPreview) {
    return trimmedPreview
  }

  const trimmedSecondary = node.secondaryLabel?.trim() ?? ''
  if (!trimmedSecondary || node.nodeType === 'document') {
    return null
  }

  const normalizedSecondary =
    node.nodeType === 'topic'
      ? humanizeGraphLabelToken(trimmedSecondary) || trimmedSecondary
      : trimmedSecondary

  return normalizedSecondary === node.label ? null : normalizedSecondary
}

function mapSearchHit(node: GraphNode, preview: string | null): GraphSearchHit {
  const resolvedPreview = resolveSearchPreview(node, preview)
  return {
    id: node.id,
    label: node.label,
    nodeType: mapSearchNodeType(node),
    secondaryLabel: resolvedPreview,
    preview: resolvedPreview,
  }
}

function normalizeSearchTerm(value: string | null | undefined): string {
  return value?.trim().toLowerCase() ?? ''
}

function scoreSearchField(
  value: string,
  query: string,
  terms: string[],
  weights: {
    exact: number
    prefix: number
    tokenPrefix: number
    includes: number
    term: number
  },
): number {
  if (!value) {
    return 0
  }

  let score = 0

  if (value === query) {
    score += weights.exact
  } else if (value.startsWith(query)) {
    score += weights.prefix
  } else if (value.includes(query)) {
    score += weights.includes
  }

  const tokens = value.split(/[\s:/._-]+/).filter(Boolean)
  if (tokens.some((token) => token.startsWith(query))) {
    score += weights.tokenPrefix
  }

  for (const term of terms) {
    if (term.length > 1 && value.includes(term)) {
      score += weights.term
    }
  }

  return score
}

function scoreSearchNode(node: GraphNode, query: string, terms: string[]): number {
  const label = normalizeSearchTerm(node.label)
  const secondary = normalizeSearchTerm(resolveSearchPreview(node, null))
  const canonical = normalizeSearchTerm(node.canonicalKey?.replace(/^[^:]+:/, ''))
  const combined = [label, secondary, canonical].filter(Boolean).join(' ')

  let score = 0
  score += scoreSearchField(label, query, terms, {
    exact: 1200,
    prefix: 780,
    tokenPrefix: 520,
    includes: 360,
    term: 84,
  })
  score += scoreSearchField(secondary, query, terms, {
    exact: 280,
    prefix: 180,
    tokenPrefix: 120,
    includes: 90,
    term: 24,
  })
  score += scoreSearchField(canonical, query, terms, {
    exact: 220,
    prefix: 150,
    tokenPrefix: 100,
    includes: 70,
    term: 18,
  })

  if (terms.length > 1 && terms.every((term) => combined.includes(term))) {
    score += 90
  }

  score += Math.min(72, Math.log2(Math.max(1, node.supportCount) + 1) * 18)
  return score
}

function buildSearchResultPreview(node: GraphNode, duplicateLabel: boolean): string | null {
  const basePreview = resolveSearchPreview(node, null)
  if (!duplicateLabel) {
    return basePreview
  }

  const supportLabel =
    node.nodeType === 'document'
      ? i18n.global.t('graph.searchRevisionCount', { count: node.supportCount })
      : i18n.global.t('graph.searchSupportCount', { count: node.supportCount })

  if (basePreview) {
    return `${basePreview} · ${supportLabel}`
  }

  return `${supportLabel} · ${compactOpaqueIdentifier(node.id)}`
}

function buildDocumentSummary(
  latestRevision: RawKnowledgeRevisionRow | null,
  latestRevisionChunks: RawKnowledgeChunkRow[],
): GraphCanonicalSummary | null {
  if (!latestRevision && latestRevisionChunks.length === 0) {
    return null
  }

  const text =
    (latestRevision?.title && latestRevision.title.length > 0 ? latestRevision.title : null) ??
    latestRevision?.normalizedText?.slice(0, 220) ??
    latestRevisionChunks.at(0)?.contentText.slice(0, 220) ??
    'Document revision'
  const state =
    latestRevision?.graphState ?? latestRevision?.vectorState ?? latestRevision?.textState
  const confidenceStatus =
    state === 'graph_ready' ? 'strong' : state === 'vector_ready' ? 'partial' : 'weak'

  return {
    text,
    confidenceStatus,
    supportCount: latestRevisionChunks.length,
    warning:
      latestRevision?.graphState === 'failed' ? 'Latest revision graph generation failed.' : null,
  }
}

function mapDocumentEvidence(
  document: RawKnowledgeDocumentRow,
  chunks: RawKnowledgeChunkRow[],
): GraphEvidence[] {
  return chunks.map((chunk) => ({
    id: chunk.chunkId,
    documentId: document.documentId,
    documentLabel: documentDisplayLabel(document),
    chunkId: chunk.chunkId,
    pageRef:
      chunk.sectionPath.length > 0
        ? chunk.sectionPath.join(' / ')
        : `chunk ${String(chunk.chunkIndex + 1)}`,
    evidenceText: chunk.contentText,
    confidenceScore: null,
    createdAt: document.updatedAt,
    activeProvenanceOnly: true,
  }))
}

function mapEntityDetail(
  entity: RawKnowledgeEntityRow,
  mentionEdges: { chunkId: string }[],
  mentionedChunks: RawKnowledgeChunkRow[],
  supportingEvidence: RawKnowledgeEvidenceRow[],
  nodes: GraphNode[],
): GraphNodeDetail {
  const label = entityDisplayLabel(entity)
  const relatedDocumentIds = new Set(mentionedChunks.map((chunk) => chunk.documentId))
  const relatedDocuments = [...relatedDocumentIds]
    .map((documentId) => findDocumentNode(nodes, documentId))
    .filter((node): node is GraphNode => Boolean(node))
    .map((node) => mapSearchHit(node, null))

  const chunkById = new Map(mentionedChunks.map((chunk) => [chunk.chunkId, chunk]))
  const relatedEdges = mentionEdges
    .map((edge) => {
      const chunk = chunkById.get(edge.chunkId)
      if (!chunk) {
        return null
      }
      const documentNode = findDocumentNode(nodes, chunk.documentId)
      return documentNode
        ? {
            id: edge.chunkId,
            relationType: 'mentions',
            otherNodeId: documentNode.id,
            otherNodeLabel: documentNode.label,
            supportCount: 1,
          }
        : null
    })
    .filter((edge): edge is NonNullable<typeof edge> => Boolean(edge))

  return {
    id: entity.entityId,
    label,
    nodeType: 'entity',
    summary: entity.summary ?? label,
    properties: [
      ['Type', entity.entityType],
      ['Support', String(entity.supportCount)],
      ['Freshness generation', String(entity.freshnessGeneration)],
      ['State', entity.entityState],
      ['Aliases', entity.aliases.length > 0 ? entity.aliases.join(', ') : '—'],
    ],
    relatedDocuments,
    connectedNodes: relatedDocuments,
    relatedEdges,
    evidence: supportingEvidence.map((evidence) => ({
      id: evidence.evidenceId,
      documentId: evidence.documentId,
      documentLabel: findDocumentNode(nodes, evidence.documentId)?.label ?? null,
      chunkId: evidence.chunkId,
      pageRef: evidence.chunkId ? `chunk ${evidence.chunkId}` : null,
      evidenceText: evidence.excerpt,
      confidenceScore: evidence.confidence,
      createdAt: evidence.createdAt,
      activeProvenanceOnly: true,
    })),
    relationCount: entity.supportCount,
    canonicalSummary: entity.summary
      ? {
          text: entity.summary,
          confidenceStatus:
            entity.confidence !== null && entity.confidence >= 0.8 ? 'strong' : 'partial',
          supportCount: entity.supportCount,
          warning: null,
        }
      : null,
    reconciliationScope: null,
    reconciliationStatus: null,
    convergenceStatus: null,
    pendingUpdateCount: 0,
    pendingDeleteCount: 0,
    activeProvenanceOnly: true,
    filteredArtifactCount: 0,
    extractionRecovery: null,
    warning: null,
  }
}

function mapRelationDetail(
  relation: RawKnowledgeRelationRow,
  supportingEvidence: RawKnowledgeEvidenceRow[],
  nodes: GraphNode[],
): GraphNodeDetail {
  const label = relationDisplayLabel(relation)
  const relatedDocuments = dedupeSearchHits(
    supportingEvidence
      .map((evidence) => findDocumentNode(nodes, evidence.documentId))
      .filter((node): node is GraphNode => Boolean(node))
      .map((node) => mapSearchHit(node, null)),
  )
  const subjectNode = relation.subjectEntityId
    ? findEntityNode(nodes, relation.subjectEntityId)
    : null
  const objectNode = relation.objectEntityId ? findEntityNode(nodes, relation.objectEntityId) : null
  const connectedNodes = dedupeSearchHits(
    [subjectNode, objectNode]
      .filter((node): node is GraphNode => Boolean(node))
      .map((node) => mapSearchHit(node, node.secondaryLabel)),
  )
  const relationLinks = [
    subjectNode
      ? {
          id: `${relation.relationId}:subject`,
          relationType: 'subject',
          otherNodeId: subjectNode.id,
          otherNodeLabel: subjectNode.label,
          supportCount: relation.supportCount,
        }
      : null,
    objectNode
      ? {
          id: `${relation.relationId}:object`,
          relationType: relation.predicate || 'object',
          otherNodeId: objectNode.id,
          otherNodeLabel: objectNode.label,
          supportCount: relation.supportCount,
        }
      : null,
  ].filter((edge): edge is NonNullable<typeof edge> => Boolean(edge))

  return {
    id: relation.relationId,
    label,
    nodeType: 'topic',
    summary: label,
    properties: [
      ['Type', relation.predicate],
      ['Assertion', label],
      ['Support', String(relation.supportCount)],
      ['Subject entity', subjectNode?.label ?? relation.subjectEntityId ?? '—'],
      ['Object entity', objectNode?.label ?? relation.objectEntityId ?? '—'],
      ['Freshness generation', String(relation.freshnessGeneration)],
      ['State', relation.relationState],
      ['Contradiction state', relation.contradictionState],
    ],
    relatedDocuments,
    connectedNodes,
    relatedEdges: [
      ...relationLinks,
      ...relatedDocuments.map((document) => ({
        id: `${relation.relationId}:${document.id}`,
        relationType: 'supported_by',
        otherNodeId: document.id,
        otherNodeLabel: document.label,
        supportCount: relation.supportCount,
      })),
    ],
    evidence: supportingEvidence.map((evidence) => ({
      id: evidence.evidenceId,
      documentId: evidence.documentId,
      documentLabel: findDocumentNode(nodes, evidence.documentId)?.label ?? null,
      chunkId: evidence.chunkId,
      pageRef: evidence.chunkId ? `chunk ${evidence.chunkId}` : null,
      evidenceText: evidence.excerpt,
      confidenceScore: evidence.confidence,
      createdAt: evidence.createdAt,
      activeProvenanceOnly: true,
    })),
    relationCount: relation.supportCount,
    canonicalSummary: {
      text: label,
      confidenceStatus:
        relation.confidence !== null && relation.confidence >= 0.8 ? 'strong' : 'partial',
      supportCount: relation.supportCount,
      warning:
        relation.contradictionState !== 'clean' &&
        relation.contradictionState !== 'resolved' &&
        relation.contradictionState !== 'none'
          ? `Relation contradiction state: ${relation.contradictionState}`
          : null,
    },
    reconciliationScope: null,
    reconciliationStatus: null,
    convergenceStatus: null,
    pendingUpdateCount: 0,
    pendingDeleteCount: 0,
    activeProvenanceOnly: true,
    filteredArtifactCount: 0,
    extractionRecovery: null,
    warning: null,
  }
}

function mapDocumentDetail(
  document: RawKnowledgeDocumentRow,
  latestRevision: RawKnowledgeRevisionRow | null,
  latestRevisionChunks: RawKnowledgeChunkRow[],
): GraphNodeDetail {
  const evidence = mapDocumentEvidence(document, latestRevisionChunks)
  const summary = buildDocumentSummary(latestRevision, latestRevisionChunks)

  return {
    id: document.documentId,
    label: documentDisplayLabel(document),
    nodeType: 'document',
    summary: summary?.text ?? documentDisplayLabel(document),
    properties: [
      ['Type', 'document'],
      ['State', document.documentState],
      ['External key', document.externalKey],
      ['Active revision', document.activeRevisionId ?? '—'],
      ['Readable revision', document.readableRevisionId ?? '—'],
      [
        'Latest revision',
        document.latestRevisionNo !== null ? String(document.latestRevisionNo) : '—',
      ],
    ],
    relatedDocuments: [],
    connectedNodes: [],
    relatedEdges: [],
    evidence,
    relationCount: 0,
    canonicalSummary: summary,
    reconciliationScope: null,
    reconciliationStatus: null,
    convergenceStatus: null,
    pendingUpdateCount: 0,
    pendingDeleteCount: 0,
    activeProvenanceOnly: true,
    filteredArtifactCount: 0,
    extractionRecovery: null,
    warning: document.deletedAt ? 'Document is deleted.' : null,
  }
}

async function fetchKnowledgeGraphTopology(
  libraryId: string,
): Promise<RawKnowledgeGraphTopologyResponse> {
  try {
    const topology = await unwrap(
      apiHttp.get<{
        documents: WireRecord[]
        entities: WireRecord[]
        relations: WireRecord[]
        documentLinks: WireRecord[]
      }>(`/knowledge/libraries/${libraryId}/graph-topology`),
    )

    return {
      documents: topology.documents.map(normalizeKnowledgeDocumentRow),
      entities: topology.entities.map(normalizeKnowledgeEntityRow),
      relations: topology.relations.map(normalizeKnowledgeRelationRow),
      documentLinks: topology.documentLinks.map(normalizeKnowledgeDocumentGraphLinkRow),
    }
  } catch (error) {
    if (error instanceof ApiClientError && error.statusCode === 404) {
      return {
        documents: [],
        entities: [],
        relations: [],
        documentLinks: [],
      }
    }
    throw error
  }
}

async function fetchKnowledgeDocumentDetail(
  libraryId: string,
  documentId: string,
): Promise<RawKnowledgeDocumentDetailResponse> {
  const detail = await unwrap(
    apiHttp.get<RawKnowledgeDocumentDetailResponse>(
      `/knowledge/libraries/${libraryId}/documents/${documentId}`,
    ),
  )
  return {
    document: normalizeKnowledgeDocumentRow(detail.document as unknown as WireRecord),
    revisions: detail.revisions.map((row) =>
      normalizeKnowledgeRevisionRow(row as unknown as WireRecord),
    ),
    latestRevision: detail.latestRevision
      ? normalizeKnowledgeRevisionRow(detail.latestRevision as unknown as WireRecord)
      : null,
    latestRevisionChunks: detail.latestRevisionChunks.map((row) =>
      normalizeKnowledgeChunkRow(row as unknown as WireRecord),
    ),
  }
}

async function fetchKnowledgeEntityDetail(
  libraryId: string,
  entityId: string,
): Promise<RawKnowledgeEntityDetailResponse> {
  const detail = await unwrap(
    apiHttp.get<RawKnowledgeEntityDetailResponse>(
      `/knowledge/libraries/${libraryId}/entities/${entityId}`,
    ),
  )
  return {
    entity: normalizeKnowledgeEntityRow(detail.entity as unknown as WireRecord),
    mentionEdges: detail.mentionEdges,
    mentionedChunks: detail.mentionedChunks.map((row) =>
      normalizeKnowledgeChunkRow(row as unknown as WireRecord),
    ),
    supportingEvidenceEdges: detail.supportingEvidenceEdges,
    supportingEvidence: detail.supportingEvidence.map((row) =>
      normalizeKnowledgeEvidenceRow(row as unknown as WireRecord),
    ),
  }
}

async function fetchKnowledgeRelationDetail(
  libraryId: string,
  relationId: string,
): Promise<RawKnowledgeRelationDetailResponse> {
  const detail = await unwrap(
    apiHttp.get<RawKnowledgeRelationDetailResponse>(
      `/knowledge/libraries/${libraryId}/relations/${relationId}`,
    ),
  )
  return {
    relation: normalizeKnowledgeRelationRow(detail.relation as unknown as WireRecord),
    supportingEvidenceEdges: detail.supportingEvidenceEdges,
    supportingEvidence: detail.supportingEvidence.map((row) =>
      normalizeKnowledgeEvidenceRow(row as unknown as WireRecord),
    ),
  }
}

export async function fetchGraphSurface(libraryId: string): Promise<GraphSurfaceResponse> {
  if (!libraryId) {
    return buildEmptySurface()
  }

  const [topology, knowledgeSummaryProjection] = await Promise.all([
    fetchKnowledgeGraphTopology(libraryId),
    resolveLibraryKnowledgeSummaryProjection(libraryId),
  ])

  const knowledgeSummary =
    knowledgeSummaryProjection.summary ?? buildEmptyLibraryKnowledgeSummary(libraryId)
  const { documents, entities, relations, documentLinks } = topology
  const nodes = [
    ...documents.map(mapDocumentRow),
    ...entities.map(mapEntityRow),
    ...relations.map(mapRelationRow),
  ]
  const latestGeneration = knowledgeSummary?.latestGeneration
    ? normalizeKnowledgeGenerationRow(knowledgeSummary.latestGeneration)
    : null
  const edges = buildGraphEdges(relations, documentLinks, nodes)
  return buildSurface(
    latestGeneration,
    nodes,
    edges,
    0,
    knowledgeSummary?.readinessSummary ?? emptyReadinessSummary(libraryId),
    knowledgeSummary?.graphCoverage ?? emptyGraphCoverage(libraryId),
    knowledgeSummaryProjection.warning,
  )
}

export async function fetchGraphSurfaceHeartbeat(
  libraryId: string,
  nodeCount: number,
  relationCount: number,
  fallback: GraphSurfaceHeartbeat | null = null,
): Promise<GraphSurfaceHeartbeat> {
  if (!libraryId) {
    const surface = buildEmptySurface()
    return {
      graphStatus: surface.graphStatus,
      convergenceStatus: surface.convergenceStatus,
      graphGeneration: surface.graphGeneration,
      graphGenerationState: surface.graphGenerationState ?? null,
      lastBuiltAt: surface.lastBuiltAt,
      readinessSummary: surface.readinessSummary,
      graphCoverage: surface.graphCoverage,
      warning: surface.warning,
    }
  }

  const knowledgeSummaryProjection = await resolveLibraryKnowledgeSummaryProjection(libraryId)
  if (knowledgeSummaryProjection.warning && fallback) {
    return {
      ...fallback,
      warning: knowledgeSummaryProjection.warning,
    }
  }
  const knowledgeSummary =
    knowledgeSummaryProjection.summary ?? buildEmptyLibraryKnowledgeSummary(libraryId)
  const latestGeneration = knowledgeSummary.latestGeneration
    ? normalizeKnowledgeGenerationRow(knowledgeSummary.latestGeneration)
    : null
  const graphStatus = mapGraphStatus(latestGeneration, nodeCount, relationCount)
  const graphGenerationState = latestGeneration?.degradedState ?? null

  return {
    graphStatus,
    convergenceStatus: mapConvergenceStatus(graphStatus, graphGenerationState),
    graphGeneration: graphGenerationOf(latestGeneration),
    graphGenerationState,
    lastBuiltAt: latestGeneration?.updatedAt ?? null,
    readinessSummary: knowledgeSummary.readinessSummary,
    graphCoverage: knowledgeSummary.graphCoverage,
    warning:
      knowledgeSummaryProjection.warning ?? projectionWarning(graphStatus, graphGenerationState),
  }
}

export async function fetchDashboardGraphDiagnostics(
  documentsSurface: DocumentsSurfaceResponse,
  libraryId?: string | null,
): Promise<GraphDiagnostics> {
  const resolvedLibraryId = libraryId ?? resolveActiveLibraryId()
  if (!resolvedLibraryId) {
    return buildGraphDiagnostics(buildEmptySurface())
  }

  const knowledgeSummaryProjection =
    await resolveLibraryKnowledgeSummaryProjection(resolvedLibraryId)
  const knowledgeSummary =
    knowledgeSummaryProjection.summary ?? buildEmptyLibraryKnowledgeSummary(resolvedLibraryId)
  const latestGeneration = knowledgeSummary?.latestGeneration
    ? normalizeKnowledgeGenerationRow(knowledgeSummary.latestGeneration)
    : null
  return buildDashboardGraphDiagnostics(
    documentsSurface,
    latestGeneration,
    knowledgeSummaryProjection.warning,
  )
}

export async function fetchGraphDiagnostics(libraryId?: string): Promise<GraphDiagnostics> {
  const resolvedLibraryId = libraryId ?? resolveActiveLibraryId()
  if (!resolvedLibraryId) {
    return buildGraphDiagnostics(buildEmptySurface())
  }

  const surface = await fetchGraphSurface(resolvedLibraryId)
  return buildGraphDiagnostics(surface)
}

function buildDashboardGraphDiagnostics(
  documentsSurface: DocumentsSurfaceResponse,
  generation: RawKnowledgeLibraryGenerationRow | null,
  summaryWarning: string | null = null,
): GraphDiagnostics {
  const readiness =
    documentsSurface.readinessSummary?.documentCountsByReadiness ??
    emptyReadinessSummary().documentCountsByReadiness
  const generationState = generation?.degradedState ?? null
  const graphStatus =
    generationState?.trim().toLowerCase() === 'rebuilding'
      ? 'rebuilding'
      : generationState?.trim().toLowerCase() === 'stale'
        ? 'stale'
        : generationState?.trim().toLowerCase() === 'failed' &&
            documentsSurface.graphStatus !== 'empty'
          ? 'failed'
          : documentsSurface.graphStatus
  const warning =
    summaryWarning ??
    projectionWarning(graphStatus, generationState) ??
    documentsSurface.graphWarning
  const blockers =
    graphStatus === 'failed'
      ? ['The canonical Arango knowledge generation failed.']
      : graphStatus === 'building' || graphStatus === 'partial'
        ? ['The canonical Arango knowledge graph is still building.']
        : graphStatus === 'rebuilding'
          ? ['The canonical Arango knowledge graph is rebuilding after recent changes.']
          : graphStatus === 'stale'
            ? ['The canonical Arango knowledge graph is stale.']
            : []

  return {
    graphStatus,
    reconciliationStatus: graphStatus === 'failed' ? 'failed' : 'current',
    convergenceStatus: mapConvergenceStatus(graphStatus, generationState),
    graphGeneration: graphGenerationOf(generation),
    nodeCount: 0,
    edgeCount: 0,
    graphFreshness:
      graphStatus === 'failed'
        ? 'failed'
        : graphStatus === 'stale'
          ? 'stale'
          : graphStatus === 'rebuilding'
            ? 'refreshing'
            : graphStatus === 'building' || graphStatus === 'partial'
              ? 'lagging'
              : 'fresh',
    rebuildBacklogCount: documentsSurface.rebuildBacklogCount,
    graphSparseCount: readiness.readable + readiness.graphSparse,
    pendingUpdateCount: 0,
    pendingDeleteCount: 0,
    activeMutationScope: null,
    filteredArtifactCount: 0,
    filteredEmptyRelationCount: 0,
    filteredDegenerateLoopCount: 0,
    provenanceCoveragePercent: null,
    lastBuiltAt: generation?.updatedAt ?? documentsSurface.graphCoverage?.updatedAt ?? null,
    lastErrorMessage: graphStatus === 'failed' ? warning : null,
    lastMutationWarning: null,
    activeProvenanceOnly: false,
    blockers,
    warning,
    graphBackend: 'canonical_arango',
  }
}

function buildGraphDiagnostics(surface: GraphSurfaceResponse): GraphDiagnostics {
  const graphStatus = surface.graphStatus
  const warning = surface.warning
  const readiness =
    surface.readinessSummary?.documentCountsByReadiness ??
    emptyReadinessSummary().documentCountsByReadiness
  const blockers =
    graphStatus === 'failed'
      ? ['The canonical Arango knowledge generation failed.']
      : graphStatus === 'building' || graphStatus === 'partial'
        ? ['The canonical Arango knowledge graph is still building.']
        : graphStatus === 'rebuilding'
          ? ['The canonical Arango knowledge graph is rebuilding after recent changes.']
          : graphStatus === 'stale'
            ? ['The canonical Arango knowledge graph is stale.']
            : []

  return {
    graphStatus,
    reconciliationStatus: graphStatus === 'failed' ? 'failed' : 'current',
    convergenceStatus: surface.convergenceStatus,
    graphGeneration: surface.graphGeneration,
    nodeCount: surface.nodeCount,
    edgeCount: surface.edgeCount,
    graphFreshness:
      graphStatus === 'failed'
        ? 'failed'
        : graphStatus === 'stale'
          ? 'stale'
          : graphStatus === 'rebuilding'
            ? 'refreshing'
            : graphStatus === 'building' || graphStatus === 'partial'
              ? 'lagging'
              : 'fresh',
    rebuildBacklogCount: readiness.processing,
    graphSparseCount: readiness.readable + readiness.graphSparse,
    pendingUpdateCount: 0,
    pendingDeleteCount: 0,
    activeMutationScope: null,
    filteredArtifactCount: 0,
    filteredEmptyRelationCount: 0,
    filteredDegenerateLoopCount: 0,
    provenanceCoveragePercent: null,
    lastBuiltAt: surface.lastBuiltAt,
    lastErrorMessage: graphStatus === 'failed' ? warning : null,
    lastMutationWarning: null,
    activeProvenanceOnly: false,
    blockers,
    warning,
    graphBackend: 'canonical_arango',
  }
}

export async function fetchGraphNodeDetail(
  libraryId: string,
  nodes: GraphNode[],
  nodeId: string,
): Promise<GraphNodeDetail | null> {
  const node = findNodeById(nodes, nodeId)
  if (!node) {
    return null
  }

  if (node.nodeType === 'document') {
    const detail = await fetchKnowledgeDocumentDetail(libraryId, node.id)
    return mapDocumentDetail(detail.document, detail.latestRevision, detail.latestRevisionChunks)
  }

  if (node.nodeType === 'entity') {
    const detail = await fetchKnowledgeEntityDetail(libraryId, node.id)
    return mapEntityDetail(
      detail.entity,
      detail.mentionEdges,
      detail.mentionedChunks,
      detail.supportingEvidence,
      nodes,
    )
  }

  const detail = await fetchKnowledgeRelationDetail(libraryId, node.id)
  return mapRelationDetail(detail.relation, detail.supportingEvidence, nodes)
}

export function searchGraphNodes(
  query: string,
  nodes: GraphNode[],
  filter: GraphNodeType | '' = '',
  limit = 8,
): GraphSearchHit[] {
  const normalizedQuery = normalizeSearchTerm(query)
  if (!normalizedQuery) {
    return []
  }

  const terms = normalizedQuery.split(/\s+/).filter(Boolean)
  const rankedNodes = nodes
    .filter((node) => !filter || node.nodeType === filter)
    .map((node) => ({
      node,
      score: scoreSearchNode(node, normalizedQuery, terms),
    }))
    .filter((candidate) => candidate.score > 0)
    .sort(
      (left, right) => right.score - left.score || left.node.label.localeCompare(right.node.label),
    )

  const labelCounts = rankedNodes.reduce<Map<string, number>>((counts, candidate) => {
    counts.set(candidate.node.label, (counts.get(candidate.node.label) ?? 0) + 1)
    return counts
  }, new Map())

  return rankedNodes
    .map((candidate) =>
      mapSearchHit(
        candidate.node,
        buildSearchResultPreview(candidate.node, (labelCounts.get(candidate.node.label) ?? 0) > 1),
      ),
    )
    .slice(0, limit)
}

export function mapGraphDiagnosticsForDashboard(diagnostics: GraphDiagnostics): {
  statusLabel: string
  attentionItem: DashboardAttentionItem | null
} {
  const statusKey = `dashboard.graphStatus.${diagnostics.graphStatus}`
  const statusLabel = i18n.global.te(statusKey) ? i18n.global.t(statusKey) : diagnostics.graphStatus

  let severity: DashboardAttentionItem['severity'] | null = null
  if (diagnostics.graphStatus === 'failed' || diagnostics.graphStatus === 'stale') {
    severity = 'error'
  } else if (diagnostics.graphStatus === 'building' || diagnostics.graphStatus === 'rebuilding') {
    severity = 'info'
  }

  return {
    statusLabel,
    attentionItem: severity
      ? {
          id: 'graph-status',
          severity,
          title: i18n.global.t('dashboard.attentionItems.graphTitle'),
          message: i18n.global.t('dashboard.attentionItems.graphMessage', { status: statusLabel }),
          targetRoute: '/graph',
          actionLabel: i18n.global.t('dashboard.attentionItems.graphAction'),
        }
      : null,
  }
}
