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
import { useShellStore } from 'src/stores/shell'
import { ApiClientError, apiHttp, unwrap } from './http'

interface RawKnowledgeDocumentRow {
  key: string
  arangoId?: string | null
  arangoRev?: string | null
  documentId: string
  workspaceId: string
  libraryId: string
  externalKey: string
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
  mentionEdges: Array<{
    key: string
    entityId: string
    chunkId: string
    rank: number | null
    score: number | null
    inclusionReason: string | null
    createdAt: string
  }>
  mentionedChunks: RawKnowledgeChunkRow[]
  supportingEvidenceEdges: Array<{
    key: string
    evidenceId: string
    entityId: string
    rank: number | null
    score: number | null
    inclusionReason: string | null
    createdAt: string
  }>
  supportingEvidence: RawKnowledgeEvidenceRow[]
}

interface RawKnowledgeRelationDetailResponse {
  relation: RawKnowledgeRelationRow
  supportingEvidenceEdges: Array<{
    key: string
    evidenceId: string
    relationId: string
    rank: number | null
    score: number | null
    inclusionReason: string | null
    createdAt: string
  }>
  supportingEvidence: RawKnowledgeEvidenceRow[]
}

interface RawKnowledgeSearchResponse {
  libraryId: string
  queryText: string
  limit: number
  embeddingProviderKind: string
  embeddingModelName: string
  embeddingModelCatalogId: string
  freshnessGeneration: number
  documentHits: Array<{
    document: RawKnowledgeDocumentRow
    revision: RawKnowledgeRevisionRow
    score: number
    lexicalRank: number | null
    vectorRank: number | null
    lexicalScore: number | null
    vectorScore: number | null
    chunkHits: Array<{
      chunkId: string
      workspaceId: string
      libraryId: string
      revisionId: string
      contentText: string
      normalizedText: string
      sectionPath: string[]
      headingTrail: string[]
      score: number
    }>
    vectorChunkHits: Array<{
      vectorId: string
      workspaceId: string
      libraryId: string
      chunkId: string
      revisionId: string
      embeddingModelKey: string
      vectorKind: string
      freshnessGeneration: number
      score: number
    }>
    evidenceSamples: RawKnowledgeEvidenceRow[]
    provenanceSummary: {
      supportingEvidenceCount: number
      lexicalChunkCount: number
      vectorChunkCount: number
    }
  }>
  entityHits: Array<{
    entityId: string
    workspaceId: string
    libraryId: string
    canonicalName: string
    entityType: string
    summary: string | null
    score: number
  }>
  relationHits: Array<{
    relationId: string
    workspaceId: string
    libraryId: string
    predicate: string
    canonicalLabel: string
    summary: string | null
    score: number
  }>
  vectorChunkHits: Array<{
    vectorId: string
    workspaceId: string
    libraryId: string
    chunkId: string
    revisionId: string
    embeddingModelKey: string
    vectorKind: string
    freshnessGeneration: number
    score: number
  }>
  vectorEntityHits: Array<{
    vectorId: string
    workspaceId: string
    libraryId: string
    entityId: string
    embeddingModelKey: string
    vectorKind: string
    freshnessGeneration: number
    score: number
  }>
}

type RawRow = Record<string, unknown>

function normalizeKnowledgeDocumentRow(row: RawRow): RawKnowledgeDocumentRow {
  return {
    key: String(row.key ?? row._key ?? ''),
    arangoId: (row.arangoId ?? row._id ?? null) as string | null,
    arangoRev: (row.arangoRev ?? row._rev ?? null) as string | null,
    documentId: String(row.documentId ?? row.document_id ?? ''),
    workspaceId: String(row.workspaceId ?? row.workspace_id ?? ''),
    libraryId: String(row.libraryId ?? row.library_id ?? ''),
    externalKey: String(row.externalKey ?? row.external_key ?? ''),
    documentState: String(row.documentState ?? row.document_state ?? ''),
    activeRevisionId: (row.activeRevisionId ?? row.active_revision_id ?? null) as string | null,
    readableRevisionId: (row.readableRevisionId ?? row.readable_revision_id ?? null) as string | null,
    latestRevisionNo: (row.latestRevisionNo ?? row.latest_revision_no ?? null) as number | null,
    createdAt: String(row.createdAt ?? row.created_at ?? ''),
    updatedAt: String(row.updatedAt ?? row.updated_at ?? ''),
    deletedAt: (row.deletedAt ?? row.deleted_at ?? null) as string | null,
  }
}

function normalizeKnowledgeRevisionRow(row: RawRow): RawKnowledgeRevisionRow {
  return {
    key: String(row.key ?? row._key ?? ''),
    arangoId: (row.arangoId ?? row._id ?? null) as string | null,
    arangoRev: (row.arangoRev ?? row._rev ?? null) as string | null,
    revisionId: String(row.revisionId ?? row.revision_id ?? ''),
    workspaceId: String(row.workspaceId ?? row.workspace_id ?? ''),
    libraryId: String(row.libraryId ?? row.library_id ?? ''),
    documentId: String(row.documentId ?? row.document_id ?? ''),
    revisionNumber: Number(row.revisionNumber ?? row.revision_number ?? 0),
    revisionState: String(row.revisionState ?? row.revision_state ?? ''),
    revisionKind: String(row.revisionKind ?? row.revision_kind ?? ''),
    storageRef: (row.storageRef ?? row.storage_ref ?? null) as string | null,
    mimeType: String(row.mimeType ?? row.mime_type ?? ''),
    checksum: String(row.checksum ?? ''),
    title: (row.title ?? null) as string | null,
    byteSize: Number(row.byteSize ?? row.byte_size ?? 0),
    normalizedText: (row.normalizedText ?? row.normalized_text ?? null) as string | null,
    textChecksum: (row.textChecksum ?? row.text_checksum ?? null) as string | null,
    textState: String(row.textState ?? row.text_state ?? ''),
    vectorState: String(row.vectorState ?? row.vector_state ?? ''),
    graphState: String(row.graphState ?? row.graph_state ?? ''),
    textReadableAt: (row.textReadableAt ?? row.text_readable_at ?? null) as string | null,
    vectorReadyAt: (row.vectorReadyAt ?? row.vector_ready_at ?? null) as string | null,
    graphReadyAt: (row.graphReadyAt ?? row.graph_ready_at ?? null) as string | null,
    supersededByRevisionId: (row.supersededByRevisionId ?? row.superseded_by_revision_id ?? null) as string | null,
    createdAt: String(row.createdAt ?? row.created_at ?? ''),
  }
}

function normalizeKnowledgeChunkRow(row: RawRow): RawKnowledgeChunkRow {
  return {
    key: String(row.key ?? row._key ?? ''),
    arangoId: (row.arangoId ?? row._id ?? null) as string | null,
    arangoRev: (row.arangoRev ?? row._rev ?? null) as string | null,
    chunkId: String(row.chunkId ?? row.chunk_id ?? ''),
    workspaceId: String(row.workspaceId ?? row.workspace_id ?? ''),
    libraryId: String(row.libraryId ?? row.library_id ?? ''),
    documentId: String(row.documentId ?? row.document_id ?? ''),
    revisionId: String(row.revisionId ?? row.revision_id ?? ''),
    chunkIndex: Number(row.chunkIndex ?? row.chunk_index ?? 0),
    contentText: String(row.contentText ?? row.content_text ?? ''),
    normalizedText: String(row.normalizedText ?? row.normalized_text ?? ''),
    spanStart: (row.spanStart ?? row.span_start ?? null) as number | null,
    spanEnd: (row.spanEnd ?? row.span_end ?? null) as number | null,
    tokenCount: (row.tokenCount ?? row.token_count ?? null) as number | null,
    sectionPath: ((row.sectionPath ?? row.section_path ?? []) as string[]),
    headingTrail: ((row.headingTrail ?? row.heading_trail ?? []) as string[]),
    chunkState: String(row.chunkState ?? row.chunk_state ?? ''),
    textGeneration: (row.textGeneration ?? row.text_generation ?? null) as number | null,
    vectorGeneration: (row.vectorGeneration ?? row.vector_generation ?? null) as number | null,
  }
}

function normalizeKnowledgeGenerationRow(row: RawRow): RawKnowledgeLibraryGenerationRow {
  return {
    key: String(row.key ?? row._key ?? ''),
    arangoId: (row.arangoId ?? row._id ?? null) as string | null,
    arangoRev: (row.arangoRev ?? row._rev ?? null) as string | null,
    generationId: String(row.generationId ?? row.generation_id ?? ''),
    workspaceId: String(row.workspaceId ?? row.workspace_id ?? ''),
    libraryId: String(row.libraryId ?? row.library_id ?? ''),
    activeTextGeneration: Number(row.activeTextGeneration ?? row.active_text_generation ?? 0),
    activeVectorGeneration: Number(row.activeVectorGeneration ?? row.active_vector_generation ?? 0),
    activeGraphGeneration: Number(row.activeGraphGeneration ?? row.active_graph_generation ?? 0),
    degradedState: String(row.degradedState ?? row.degraded_state ?? ''),
    updatedAt: String(row.updatedAt ?? row.updated_at ?? ''),
  }
}

function normalizeKnowledgeEntityRow(row: RawRow): RawKnowledgeEntityRow {
  return {
    key: String(row.key ?? row._key ?? ''),
    arangoId: (row.arangoId ?? row._id ?? null) as string | null,
    arangoRev: (row.arangoRev ?? row._rev ?? null) as string | null,
    entityId: String(row.entityId ?? row.entity_id ?? ''),
    workspaceId: String(row.workspaceId ?? row.workspace_id ?? ''),
    libraryId: String(row.libraryId ?? row.library_id ?? ''),
    canonicalLabel: String(row.canonicalLabel ?? row.canonical_label ?? ''),
    aliases: (row.aliases ?? []) as string[],
    entityType: String(row.entityType ?? row.entity_type ?? ''),
    summary: (row.summary ?? null) as string | null,
    confidence: (row.confidence ?? null) as number | null,
    supportCount: Number(row.supportCount ?? row.support_count ?? 0),
    freshnessGeneration: Number(row.freshnessGeneration ?? row.freshness_generation ?? 0),
    entityState: String(row.entityState ?? row.entity_state ?? ''),
    createdAt: String(row.createdAt ?? row.created_at ?? ''),
    updatedAt: String(row.updatedAt ?? row.updated_at ?? ''),
  }
}

function normalizeKnowledgeRelationRow(row: RawRow): RawKnowledgeRelationRow {
  return {
    key: String(row.key ?? row._key ?? ''),
    arangoId: (row.arangoId ?? row._id ?? null) as string | null,
    arangoRev: (row.arangoRev ?? row._rev ?? null) as string | null,
    relationId: String(row.relationId ?? row.relation_id ?? ''),
    workspaceId: String(row.workspaceId ?? row.workspace_id ?? ''),
    libraryId: String(row.libraryId ?? row.library_id ?? ''),
    predicate: String(row.predicate ?? ''),
    normalizedAssertion: String(row.normalizedAssertion ?? row.normalized_assertion ?? ''),
    confidence: (row.confidence ?? null) as number | null,
    supportCount: Number(row.supportCount ?? row.support_count ?? 0),
    contradictionState: String(row.contradictionState ?? row.contradiction_state ?? ''),
    freshnessGeneration: Number(row.freshnessGeneration ?? row.freshness_generation ?? 0),
    relationState: String(row.relationState ?? row.relation_state ?? ''),
    subjectEntityId: (row.subjectEntityId ?? row.subject_entity_id ?? null) as string | null,
    objectEntityId: (row.objectEntityId ?? row.object_entity_id ?? null) as string | null,
    createdAt: String(row.createdAt ?? row.created_at ?? ''),
    updatedAt: String(row.updatedAt ?? row.updated_at ?? ''),
  }
}

function normalizeKnowledgeEvidenceRow(row: RawRow): RawKnowledgeEvidenceRow {
  return {
    key: String(row.key ?? row._key ?? ''),
    arangoId: (row.arangoId ?? row._id ?? null) as string | null,
    arangoRev: (row.arangoRev ?? row._rev ?? null) as string | null,
    evidenceId: String(row.evidenceId ?? row.evidence_id ?? ''),
    workspaceId: String(row.workspaceId ?? row.workspace_id ?? ''),
    libraryId: String(row.libraryId ?? row.library_id ?? ''),
    documentId: String(row.documentId ?? row.document_id ?? ''),
    revisionId: String(row.revisionId ?? row.revision_id ?? ''),
    chunkId: (row.chunkId ?? row.chunk_id ?? null) as string | null,
    spanStart: (row.spanStart ?? row.span_start ?? null) as number | null,
    spanEnd: (row.spanEnd ?? row.span_end ?? null) as number | null,
    excerpt: String(row.excerpt ?? ''),
    supportKind: String(row.supportKind ?? row.support_kind ?? ''),
    extractionMethod: String(row.extractionMethod ?? row.extraction_method ?? ''),
    confidence: (row.confidence ?? null) as number | null,
    evidenceState: String(row.evidenceState ?? row.evidence_state ?? ''),
    freshnessGeneration: Number(row.freshnessGeneration ?? row.freshness_generation ?? 0),
    createdAt: String(row.createdAt ?? row.created_at ?? ''),
    updatedAt: String(row.updatedAt ?? row.updated_at ?? ''),
  }
}

function resolveActiveLibraryId(): string | null {
  return useShellStore().context?.activeLibrary.id ?? null
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
    graphStatus: 'empty',
    convergenceStatus: null,
    graphGeneration: 0,
    graphGenerationState: null,
    nodeCount: 0,
    relationCount: 0,
    filteredArtifactCount: 0,
    lastBuiltAt: null,
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
      return 'building'
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
  if (
    graphStatus === 'partial' ||
    graphStatus === 'rebuilding' ||
    generationState === 'accepted'
  ) {
    return 'partial'
  }
  if (graphStatus === 'failed' || graphStatus === 'stale') {
    return 'degraded'
  }
  return null
}

function projectionWarning(graphStatus: GraphStatus, generationState: string | null): string | null {
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
): GraphSurfaceResponse {
  const relationNodeCount = rawNodes.filter((node) => node.nodeType === 'topic').length
  const graphStatus = mapGraphStatus(generation, rawNodes.length, relationNodeCount)
  const generationState = generation?.degradedState ?? null

  return {
    graphStatus,
    convergenceStatus: mapConvergenceStatus(graphStatus, generationState),
    graphGeneration: graphGenerationOf(generation),
    graphGenerationState: generationState,
    nodeCount: rawNodes.length,
    relationCount: relationNodeCount,
    filteredArtifactCount: 0,
    lastBuiltAt: generation?.updatedAt ?? null,
    warning: projectionWarning(graphStatus, generationState),
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

function findRelationNode(nodes: GraphNode[], relationId: string): GraphNode | null {
  return (
    nodes.find((node) => node.id === relationId) ??
    nodes.find((node) => node.canonicalKey === `relation:${relationId}`) ??
    null
  )
}

function mapDocumentRow(row: RawKnowledgeDocumentRow): GraphNode {
  return {
    id: row.documentId,
    canonicalKey: `document:${row.documentId}`,
    label: row.externalKey,
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
    label: row.canonicalLabel,
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
    label: row.normalizedAssertion,
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

function buildRelationEdges(relations: RawKnowledgeRelationRow[], nodes: GraphNode[]): GraphEdge[] {
  const nodeIds = new Set(nodes.map((node) => node.id))
  return relations.flatMap((relation) => {
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
}

function mapSearchNodeType(node: GraphNode): GraphNodeType {
  return node.nodeType
}

function mapSearchHit(node: GraphNode, preview: string | null): GraphSearchHit {
  return {
    id: node.id,
    label: node.label,
    nodeType: mapSearchNodeType(node),
    secondaryLabel: preview ?? node.secondaryLabel,
    preview,
  }
}

function buildDocumentSummary(
  latestRevision: RawKnowledgeRevisionRow | null,
  latestRevisionChunks: RawKnowledgeChunkRow[],
): GraphCanonicalSummary | null {
  if (!latestRevision && latestRevisionChunks.length === 0) {
    return null
  }

  const text =
    latestRevision?.title ??
    latestRevision?.normalizedText?.slice(0, 220) ??
    latestRevisionChunks[0]?.contentText.slice(0, 220) ??
    'Document revision'
  const state = latestRevision?.graphState ?? latestRevision?.vectorState ?? latestRevision?.textState
  const confidenceStatus =
    state === 'graph_ready'
      ? 'strong'
      : state === 'vector_ready'
        ? 'partial'
        : 'weak'

  return {
    text,
    confidenceStatus,
    supportCount: latestRevisionChunks.length,
    warning: latestRevision?.graphState === 'failed' ? 'Latest revision graph generation failed.' : null,
  }
}

function mapDocumentEvidence(
  document: RawKnowledgeDocumentRow,
  chunks: RawKnowledgeChunkRow[],
): GraphEvidence[] {
  return chunks.map((chunk) => ({
    id: chunk.chunkId,
    documentId: document.documentId,
    documentLabel: document.externalKey,
    chunkId: chunk.chunkId,
    pageRef: chunk.sectionPath.length > 0 ? chunk.sectionPath.join(' / ') : `chunk ${chunk.chunkIndex + 1}`,
    evidenceText: chunk.contentText,
    confidenceScore: null,
    createdAt: document.updatedAt,
    activeProvenanceOnly: true,
  }))
}

function mapEntityDetail(
  entity: RawKnowledgeEntityRow,
  mentionEdges: Array<{ chunkId: string }>,
  mentionedChunks: RawKnowledgeChunkRow[],
  supportingEvidence: RawKnowledgeEvidenceRow[],
  nodes: GraphNode[],
): GraphNodeDetail {
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
    label: entity.canonicalLabel,
    nodeType: 'entity',
    summary: entity.summary ?? entity.canonicalLabel,
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
          confidenceStatus: entity.confidence !== null && entity.confidence >= 0.8 ? 'strong' : 'partial',
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
  const relatedDocuments = dedupeSearchHits(
    supportingEvidence
    .map((evidence) => findDocumentNode(nodes, evidence.documentId))
    .filter((node): node is GraphNode => Boolean(node))
    .map((node) => mapSearchHit(node, null))
  )
  const subjectNode = relation.subjectEntityId ? findEntityNode(nodes, relation.subjectEntityId) : null
  const objectNode = relation.objectEntityId ? findEntityNode(nodes, relation.objectEntityId) : null
  const connectedNodes = dedupeSearchHits(
    [subjectNode, objectNode]
      .filter((node): node is GraphNode => Boolean(node))
      .map((node) => mapSearchHit(node, node.secondaryLabel))
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
    label: relation.normalizedAssertion,
    nodeType: 'topic',
    summary: relation.normalizedAssertion,
    properties: [
      ['Type', relation.predicate],
      ['Assertion', relation.normalizedAssertion],
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
      text: relation.normalizedAssertion,
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
    label: document.externalKey,
    nodeType: 'document',
    summary: summary?.text ?? document.externalKey,
    properties: [
      ['Type', 'document'],
      ['State', document.documentState],
      ['External key', document.externalKey],
      ['Active revision', document.activeRevisionId ?? '—'],
      ['Readable revision', document.readableRevisionId ?? '—'],
      ['Latest revision', document.latestRevisionNo !== null ? String(document.latestRevisionNo) : '—'],
    ],
    relatedDocuments: [],
    connectedNodes: [],
    relatedEdges: [],
    evidence,
    relationCount: latestRevisionChunks.length,
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

async function fetchKnowledgeDocuments(libraryId: string): Promise<RawKnowledgeDocumentRow[]> {
  try {
    const rows = await unwrap(apiHttp.get<RawRow[]>(`/knowledge/libraries/${libraryId}/documents`))
    return rows.map(normalizeKnowledgeDocumentRow)
  } catch (error) {
    if (error instanceof ApiClientError && error.statusCode === 404) {
      return []
    }
    throw error
  }
}

async function fetchKnowledgeEntities(libraryId: string): Promise<RawKnowledgeEntityRow[]> {
  try {
    const rows = await unwrap(apiHttp.get<RawRow[]>(`/knowledge/libraries/${libraryId}/entities`))
    return rows.map(normalizeKnowledgeEntityRow)
  } catch (error) {
    if (error instanceof ApiClientError && error.statusCode === 404) {
      return []
    }
    throw error
  }
}

async function fetchKnowledgeRelations(libraryId: string): Promise<RawKnowledgeRelationRow[]> {
  try {
    const rows = await unwrap(apiHttp.get<RawRow[]>(`/knowledge/libraries/${libraryId}/relations`))
    return rows.map(normalizeKnowledgeRelationRow)
  } catch (error) {
    if (error instanceof ApiClientError && error.statusCode === 404) {
      return []
    }
    throw error
  }
}

async function fetchKnowledgeGenerations(
  libraryId: string,
): Promise<RawKnowledgeLibraryGenerationRow[]> {
  try {
    const rows = await unwrap(
      apiHttp.get<RawRow[]>(`/knowledge/libraries/${libraryId}/generations`),
    )
    return rows.map(normalizeKnowledgeGenerationRow)
  } catch (error) {
    if (error instanceof ApiClientError && error.statusCode === 404) {
      return []
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
    document: normalizeKnowledgeDocumentRow(detail.document as unknown as RawRow),
    revisions: detail.revisions.map((row) => normalizeKnowledgeRevisionRow(row as unknown as RawRow)),
    latestRevision: detail.latestRevision
      ? normalizeKnowledgeRevisionRow(detail.latestRevision as unknown as RawRow)
      : null,
    latestRevisionChunks: detail.latestRevisionChunks.map((row) =>
      normalizeKnowledgeChunkRow(row as unknown as RawRow),
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
    entity: normalizeKnowledgeEntityRow(detail.entity as unknown as RawRow),
    mentionEdges: detail.mentionEdges,
    mentionedChunks: detail.mentionedChunks.map((row) =>
      normalizeKnowledgeChunkRow(row as unknown as RawRow),
    ),
    supportingEvidenceEdges: detail.supportingEvidenceEdges,
    supportingEvidence: detail.supportingEvidence.map((row) =>
      normalizeKnowledgeEvidenceRow(row as unknown as RawRow),
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
    relation: normalizeKnowledgeRelationRow(detail.relation as unknown as RawRow),
    supportingEvidenceEdges: detail.supportingEvidenceEdges,
    supportingEvidence: detail.supportingEvidence.map((row) =>
      normalizeKnowledgeEvidenceRow(row as unknown as RawRow),
    ),
  }
}

async function fetchKnowledgeSearch(
  libraryId: string,
  query: string,
  limit: number,
): Promise<RawKnowledgeSearchResponse> {
  const response = await unwrap(
    apiHttp.get<RawKnowledgeSearchResponse>(`/knowledge/libraries/${libraryId}/search/documents`, {
      params: { query, limit },
    }),
  )
  return {
    ...response,
    documentHits: response.documentHits.map((hit) => ({
      ...hit,
      document: normalizeKnowledgeDocumentRow(hit.document as unknown as RawRow),
      revision: normalizeKnowledgeRevisionRow(hit.revision as unknown as RawRow),
      chunkHits: hit.chunkHits.map((row) => {
        const normalized = normalizeKnowledgeChunkRow(row as unknown as RawRow)
        return {
          chunkId: normalized.chunkId,
          workspaceId: normalized.workspaceId,
          libraryId: normalized.libraryId,
          revisionId: normalized.revisionId,
          contentText: normalized.contentText,
          normalizedText: normalized.normalizedText,
          sectionPath: normalized.sectionPath,
          headingTrail: normalized.headingTrail,
          score: row.score,
        }
      }),
      evidenceSamples: hit.evidenceSamples.map((row) =>
        normalizeKnowledgeEvidenceRow(row as unknown as RawRow),
      ),
    })),
    relationHits: response.relationHits.map((row) => ({
      ...row,
      relationId: row.relationId ?? (row as unknown as { relation_id?: string }).relation_id ?? '',
      workspaceId: row.workspaceId ?? (row as unknown as { workspace_id?: string }).workspace_id ?? '',
      libraryId: row.libraryId ?? (row as unknown as { library_id?: string }).library_id ?? '',
      predicate: row.predicate,
      canonicalLabel:
        row.canonicalLabel ??
        (row as unknown as { canonical_label?: string; normalized_assertion?: string }).canonical_label ??
        (row as unknown as { normalized_assertion?: string }).normalized_assertion ??
        '',
      summary: row.summary ?? null,
      score: row.score,
    })),
  }
}

export async function fetchGraphSurface(libraryId: string): Promise<GraphSurfaceResponse> {
  if (!libraryId) {
    return buildEmptySurface()
  }

  const [documents, entities, relations, generations] = await Promise.all([
    fetchKnowledgeDocuments(libraryId),
    fetchKnowledgeEntities(libraryId),
    fetchKnowledgeRelations(libraryId),
    fetchKnowledgeGenerations(libraryId),
  ])

  const nodes = [
    ...documents.map(mapDocumentRow),
    ...entities.map(mapEntityRow),
    ...relations.map(mapRelationRow),
  ]
  const latestGeneration = generations[0] ?? null
  const edges = buildRelationEdges(relations, nodes)
  return buildSurface(latestGeneration, nodes, edges)
}

export async function fetchGraphDiagnostics(libraryId?: string): Promise<GraphDiagnostics> {
  const resolvedLibraryId = libraryId ?? resolveActiveLibraryId()
  if (!resolvedLibraryId) {
    return buildGraphDiagnostics(buildEmptySurface())
  }

  const surface = await fetchGraphSurface(resolvedLibraryId)
  return buildGraphDiagnostics(surface)
}

function buildGraphDiagnostics(surface: GraphSurfaceResponse): GraphDiagnostics {
  const graphStatus = surface.graphStatus
  const warning = surface.warning
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
    edgeCount: surface.edges.length,
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
    rebuildBacklogCount: 0,
    readyNoGraphCount: 0,
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

export async function searchGraphNodes(
  libraryId: string,
  query: string,
  nodes: GraphNode[],
  limit = 8,
): Promise<GraphSearchHit[]> {
  const trimmed = query.trim()
  if (!trimmed) {
    return []
  }

  const response = await fetchKnowledgeSearch(libraryId, trimmed, limit)
  const scores = new Map<string, number>()
  const hits: GraphSearchHit[] = []

  for (const hit of response.documentHits) {
    const node = findDocumentNode(nodes, hit.document.documentId)
    if (!node) {
      continue
    }
    scores.set(node.id, hit.score)
    hits.push(
      mapSearchHit(node, hit.evidenceSamples[0]?.excerpt ?? hit.revision.title ?? hit.document.externalKey),
    )
  }

  for (const hit of response.entityHits) {
    const node = findEntityNode(nodes, hit.entityId)
    if (!node) {
      continue
    }
    scores.set(node.id, hit.score)
    hits.push(mapSearchHit(node, hit.summary ?? hit.canonicalName))
  }

  for (const hit of response.relationHits) {
    const node = findRelationNode(nodes, hit.relationId)
    if (!node) {
      continue
    }
    scores.set(node.id, hit.score)
    hits.push(mapSearchHit(node, hit.summary ?? hit.canonicalLabel))
  }

  return hits
    .sort((left, right) => (scores.get(right.id) ?? 0) - (scores.get(left.id) ?? 0))
    .slice(0, limit)
}
