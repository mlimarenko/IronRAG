import type {
  GraphConvergenceStatus,
  GraphDiagnostics,
  GraphEdge,
  GraphLegendItem,
  GraphNode,
  GraphNodeType,
  GraphSearchHit,
  GraphStatus,
  GraphSurfaceResponse,
} from 'src/models/ui/graph'
import { useShellStore } from 'src/stores/shell'
import { ApiClientError, apiHttp, unwrap } from './http'

interface RawGraphProjectionResponse {
  id: string
  workspaceId: string
  libraryId: string
  sourceAttemptId: string | null
  projectionState: string
  startedAt: string
  completedAt: string | null
}

interface RawGraphNode {
  id: string
  projectionId: string
  canonicalKey: string
  nodeKind: string
  displayLabel: string
  supportCount: number
}

interface RawGraphEdge {
  id: string
  projectionId: string
  canonicalKey: string
  edgeKind: string
  fromNodeId: string
  toNodeId: string
  supportCount: number
}

interface RawSearchHit {
  subjectId: string
  score: number
  preview: string | null
}

function resolveActiveLibraryId(): string | null {
  return useShellStore().context?.activeLibrary.id ?? null
}

function mapNodeType(nodeKind: string): GraphNodeType {
  const normalized = nodeKind.trim().toLowerCase()
  if (normalized === 'document') {
    return 'document'
  }
  if (normalized === 'topic' || normalized === 'concept' || normalized === 'theme') {
    return 'topic'
  }
  return 'entity'
}

function mapGraphStatus(
  projectionState: string,
  nodeCount: number,
  edgeCount: number,
): GraphStatus {
  switch (projectionState) {
    case 'building':
      return nodeCount > 0 || edgeCount > 0 ? 'partial' : 'building'
    case 'failed':
      return 'failed'
    case 'superseded':
      return 'stale'
    case 'active':
      return nodeCount > 0 || edgeCount > 0 ? 'ready' : 'empty'
    default:
      return nodeCount > 0 || edgeCount > 0 ? 'ready' : 'empty'
  }
}

function mapConvergenceStatus(
  graphStatus: GraphStatus,
  projectionState: string,
): GraphConvergenceStatus | null {
  if (graphStatus === 'ready') {
    return 'current'
  }
  if (graphStatus === 'partial' || projectionState === 'building') {
    return 'partial'
  }
  if (graphStatus === 'failed' || graphStatus === 'stale') {
    return 'degraded'
  }
  return null
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
    items.push({ key: 'topic', label: 'Topic' })
  }
  if (edgeCount > 0) {
    items.push({ key: 'relation', label: 'Relation' })
  }

  return items
}

function mapNode(row: RawGraphNode): GraphNode {
  return {
    id: row.id,
    canonicalKey: row.canonicalKey,
    label: row.displayLabel,
    nodeType: mapNodeType(row.nodeKind),
    secondaryLabel: null,
    supportCount: row.supportCount,
    filteredArtifact: false,
  }
}

function mapEdge(row: RawGraphEdge): GraphEdge {
  return {
    id: row.id,
    canonicalKey: row.canonicalKey,
    source: row.fromNodeId,
    target: row.toNodeId,
    relationType: row.edgeKind,
    supportCount: row.supportCount,
    filteredArtifact: false,
  }
}

function projectionVersionOf(projection: RawGraphProjectionResponse | null): number {
  if (!projection) {
    return 0
  }
  const completedAt = projection.completedAt ? Date.parse(projection.completedAt) : Number.NaN
  if (Number.isFinite(completedAt)) {
    return completedAt
  }
  const startedAt = Date.parse(projection.startedAt)
  return Number.isFinite(startedAt) ? startedAt : 0
}

function projectionWarning(graphStatus: GraphStatus): string | null {
  if (graphStatus === 'failed') {
    return 'The latest canonical graph projection failed.'
  }
  if (graphStatus === 'building' || graphStatus === 'partial') {
    return 'The canonical graph projection is still building.'
  }
  if (graphStatus === 'stale') {
    return 'The canonical graph projection is stale.'
  }
  return null
}

function buildEmptySurface(): GraphSurfaceResponse {
  return {
    graphStatus: 'empty',
    convergenceStatus: null,
    projectionVersion: 0,
    projectionState: null,
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

function buildSurface(
  projection: RawGraphProjectionResponse | null,
  rawNodes: RawGraphNode[],
  rawEdges: RawGraphEdge[],
): GraphSurfaceResponse {
  const nodes = rawNodes.map(mapNode)
  const edges = rawEdges.map(mapEdge)
  const graphStatus = projection
    ? mapGraphStatus(projection.projectionState, nodes.length, edges.length)
    : 'empty'

  return {
    graphStatus,
    convergenceStatus: projection
      ? mapConvergenceStatus(graphStatus, projection.projectionState)
      : null,
    projectionVersion: projectionVersionOf(projection),
    projectionState: projection?.projectionState ?? null,
    nodeCount: nodes.length,
    relationCount: edges.length,
    filteredArtifactCount: 0,
    lastBuiltAt: projection?.completedAt ?? projection?.startedAt ?? null,
    warning: projectionWarning(graphStatus),
    nodes,
    edges,
    legend: buildLegend(nodes, edges.length),
  }
}

function resolveDocumentNode(nodes: GraphNode[], documentId: string): GraphNode | null {
  return (
    nodes.find((node) => node.id === documentId) ??
    nodes.find((node) => node.canonicalKey === `document:${documentId}`) ??
    null
  )
}

function buildGraphDiagnostics(surface: GraphSurfaceResponse): GraphDiagnostics {
  const graphStatus = surface.graphStatus
  const warning = surface.warning
  const blockers =
    graphStatus === 'failed'
      ? ['The latest canonical graph projection failed.']
      : graphStatus === 'building' || graphStatus === 'partial'
        ? ['The canonical graph projection is still building.']
        : graphStatus === 'stale'
          ? ['The canonical graph projection is stale.']
          : []

  return {
    graphStatus,
    reconciliationStatus: graphStatus === 'failed' ? 'failed' : 'current',
    convergenceStatus: surface.convergenceStatus,
    projectionVersion: surface.projectionVersion,
    nodeCount: surface.nodeCount,
    edgeCount: surface.relationCount,
    projectionFreshness:
      graphStatus === 'failed'
        ? 'failed'
        : graphStatus === 'stale'
          ? 'stale'
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
    graphBackend: 'canonical_sql',
  }
}

export async function fetchGraphProjection(
  libraryId: string,
): Promise<RawGraphProjectionResponse | null> {
  try {
    return await unwrap(
      apiHttp.get<RawGraphProjectionResponse>(`/graph/libraries/${libraryId}/projection`),
    )
  } catch (error) {
    if (error instanceof ApiClientError && error.statusCode === 404) {
      return null
    }
    throw error
  }
}

export async function fetchGraphNodes(libraryId: string): Promise<RawGraphNode[]> {
  try {
    return await unwrap(apiHttp.get<RawGraphNode[]>(`/graph/libraries/${libraryId}/nodes`))
  } catch (error) {
    if (error instanceof ApiClientError && error.statusCode === 404) {
      return []
    }
    throw error
  }
}

export async function fetchGraphEdges(libraryId: string): Promise<RawGraphEdge[]> {
  try {
    return await unwrap(apiHttp.get<RawGraphEdge[]>(`/graph/libraries/${libraryId}/edges`))
  } catch (error) {
    if (error instanceof ApiClientError && error.statusCode === 404) {
      return []
    }
    throw error
  }
}

export async function fetchGraphSurface(libraryId: string): Promise<GraphSurfaceResponse> {
  if (!libraryId) {
    return buildEmptySurface()
  }

  const projection = await fetchGraphProjection(libraryId)
  if (!projection) {
    return buildEmptySurface()
  }

  const [nodes, edges] = await Promise.all([fetchGraphNodes(libraryId), fetchGraphEdges(libraryId)])
  return buildSurface(projection, nodes, edges)
}

export async function fetchGraphDiagnostics(libraryId?: string): Promise<GraphDiagnostics> {
  const resolvedLibraryId = libraryId ?? resolveActiveLibraryId()
  if (!resolvedLibraryId) {
    return buildGraphDiagnostics(buildEmptySurface())
  }

  const surface = await fetchGraphSurface(resolvedLibraryId)
  return buildGraphDiagnostics(surface)
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

  const hits = await unwrap(
    apiHttp.post<RawSearchHit[]>('/search/documents', {
      libraryId,
      queryText: trimmed,
      limit,
    }),
  )

  const mappedHits: GraphSearchHit[] = []
  for (const hit of hits) {
    const node = resolveDocumentNode(nodes, hit.subjectId)
    if (!node) {
      continue
    }

    mappedHits.push({
      id: node.id,
      label: node.label,
      nodeType: 'document',
      secondaryLabel: hit.preview,
      preview: hit.preview,
    })
  }

  return mappedHits
}
