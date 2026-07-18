import type { KnowledgeGraphTopologyResponse } from '@/shared/types/graph-topology'
import type { GraphLayoutType } from '@/features/graph/model/config'
import type {
  ContentDocumentDetailResponse,
  ContentRevision,
  KnowledgeEntityDetailResponse,
} from '@/shared/api/generated'
import type { GraphEdge, GraphMetadata, GraphNode, GraphNodeType } from '@/shared/types'

type GraphTopology = {
  nodes: GraphNode[]
  edges: GraphEdge[]
  meta: GraphMetadata
}

function mapNodeType(t: string | undefined): GraphNodeType {
  if (t === 'document') return 'document'
  if (t === 'person') return 'person'
  if (t === 'organization') return 'organization'
  if (t === 'location') return 'location'
  if (t === 'event') return 'event'
  if (t === 'artifact') return 'artifact'
  if (t === 'natural') return 'natural'
  if (t === 'process') return 'process'
  if (t === 'concept') return 'concept'
  if (t === 'attribute') return 'attribute'
  return 'entity'
}

function addUnvisitedNeighbors(
  current: string,
  adjacency: ReadonlyMap<string, string[]>,
  visited: Set<string>,
  queue: string[],
): void {
  for (const neighbor of adjacency.get(current) ?? []) {
    if (visited.has(neighbor)) continue
    visited.add(neighbor)
    queue.push(neighbor)
  }
}

function buildAdjacency(nodes: GraphNode[], edges: GraphEdge[]): Map<string, string[]> {
  const adjacency = new Map(nodes.map((node) => [node.id, [] as string[]]))
  for (const edge of edges) {
    if (edge.sourceId === edge.targetId) continue
    const sourceNeighbors = adjacency.get(edge.sourceId)
    const targetNeighbors = adjacency.get(edge.targetId)
    if (!sourceNeighbors || !targetNeighbors) continue
    sourceNeighbors.push(edge.targetId)
    targetNeighbors.push(edge.sourceId)
  }
  return adjacency
}

function visitConnectedComponent(
  firstNodeId: string,
  adjacency: ReadonlyMap<string, string[]>,
  visited: Set<string>,
): void {
  const queue = [firstNodeId]
  visited.add(firstNodeId)
  for (const current of queue) {
    addUnvisitedNeighbors(current, adjacency, visited, queue)
  }
}

function countConnectedComponents(nodes: GraphNode[], edges: GraphEdge[]): number {
  const adjacency = buildAdjacency(nodes, edges)
  const visited = new Set<string>()
  let componentCount = 0
  for (const node of nodes) {
    if (visited.has(node.id)) continue
    componentCount += 1
    visitConnectedComponent(node.id, adjacency, visited)
  }
  return componentCount
}

function recommendGraphLayout(nodes: GraphNode[], edges: GraphEdge[]): GraphLayoutType {
  if (nodes.length === 0) return 'sectors'

  const typeCount = new Set(nodes.map((node) => node.type)).size
  const documentCount = nodes.reduce((count, node) => count + (node.type === 'document' ? 1 : 0), 0)
  const componentCount = countConnectedComponents(nodes, edges)
  const edgeDensity = edges.length / nodes.length

  if (componentCount >= 6 && edges.length < nodes.length * 2.2) {
    return 'circlepack'
  }

  // Force-directed layout best conveys "distance ~ connection strength" (the
  // LightRAG-style reading) for small/mid CONNECTED graphs. Gate it to sizes
  // where the iterative worker cost stays modest and to graphs that are
  // actually linked — enough edges, not shattered into islands. Larger or
  // fragmented graphs fall through to the geometric recommendations below.
  if (
    nodes.length >= 12 &&
    nodes.length <= 1500 &&
    edges.length >= nodes.length * 0.9 &&
    componentCount <= Math.max(3, Math.round(nodes.length / 25))
  ) {
    return 'force'
  }

  if (nodes.length > 2000 || edgeDensity > 3.2) {
    if (edgeDensity > 4.2) return 'hubs'
    if (documentCount > 0) return 'sources'
    return 'radial'
  }

  if (documentCount > 0 && nodes.length > 120) {
    return 'sources'
  }

  if (typeCount >= 6) {
    return 'circlepack'
  }

  if (edgeDensity > 2.4) {
    return 'hubs'
  }

  return 'sectors'
}

export function mapGraphTopology(topology: KnowledgeGraphTopologyResponse): GraphTopology {
  const { entities, relations, documents, documentLinks } = topology

  const relationEdges: GraphEdge[] = relations
    .map((r): GraphEdge | null => {
      if (!r.subjectEntityId || !r.objectEntityId) return null
      return {
        id: r.relationId ?? r.id ?? '',
        sourceId: r.subjectEntityId,
        targetId: r.objectEntityId,
        label: r.predicate ?? '',
        weight: r.supportCount ?? 1,
      }
    })
    .filter((edge): edge is GraphEdge => edge !== null)

  const documentEdges: GraphEdge[] = documentLinks.map((link) => ({
    id: `dl-${link.documentId}-${link.targetNodeId}`,
    sourceId: link.documentId,
    targetId: link.targetNodeId,
    label: 'supports',
    weight: link.supportCount ?? 1,
  }))

  const edges: GraphEdge[] = [...relationEdges, ...documentEdges]

  // Node `edgeCount` is graph DEGREE (distinct neighbours) for EVERY node
  // type, so the visual size channel, label ranking, and inspector badge all
  // encode connectivity consistently — matching the tooltip's neighbour count
  // and the inspector's live adjacency total. Previously entities carried
  // `supportCount` here while documents carried their link count, so one
  // visual channel meant two different things. `supportCount` still surfaces
  // on the entity detail view (its `properties['support count']`).
  const neighboursByNode = new Map<string, Set<string>>()
  const linkNeighbour = (a: string, b: string) => {
    if (!a || !b || a === b) return
    let neighbours = neighboursByNode.get(a)
    if (!neighbours) {
      neighbours = new Set()
      neighboursByNode.set(a, neighbours)
    }
    neighbours.add(b)
  }
  for (const edge of edges) {
    linkNeighbour(edge.sourceId, edge.targetId)
    linkNeighbour(edge.targetId, edge.sourceId)
  }
  const degreeOf = (id: string): number => neighboursByNode.get(id)?.size ?? 0

  const entityNodes: GraphNode[] = entities.map((e) => {
    const canonical = mapNodeType(e.entityType)
    const rawType = (e.entityType ?? '').toLowerCase()
    const id = e.entityId ?? e.id ?? ''
    const node: GraphNode = {
      id,
      label: e.canonicalLabel ?? e.label ?? e.key ?? 'unknown',
      type: canonical,
      edgeCount: degreeOf(id),
      properties: {},
      sourceDocumentIds: [],
    }
    const subType = e.entitySubType ?? (rawType !== canonical ? rawType : undefined)
    if (subType !== undefined) node.subType = subType
    if (e.summary !== undefined && e.summary !== null) node.summary = e.summary
    return node
  })

  const documentNodes: GraphNode[] = documents.map((d) => {
    const docId = d.document_id ?? d.documentId ?? d.id ?? ''
    return {
      id: docId,
      label: d.title ?? d.fileName ?? d.external_key ?? 'untitled',
      type: 'document',
      edgeCount: degreeOf(docId),
      properties: {},
      sourceDocumentIds: [],
    }
  })

  const nodes: GraphNode[] = [...entityNodes, ...documentNodes]

  const recommendedLayout = recommendGraphLayout(nodes, edges)
  const status = topology.status ?? (nodes.length > 0 ? 'ready' : 'empty')

  const meta: GraphMetadata = {
    nodeCount: nodes.length,
    edgeCount: edges.length,
    hiddenDisconnectedCount: 0,
    status,
    convergenceStatus: topology.convergenceStatus ?? 'current',
    recommendedLayout,
  }

  return { nodes, edges, meta }
}

/**
 * Map the entity-detail response onto the inspector's `GraphNode` view model.
 * The list node supplies only UI continuity values while the generated detail
 * transport stays the source of truth for graph evidence and entity metadata.
 */
export function mapKnowledgeEntityDetail(
  raw: KnowledgeEntityDetailResponse,
  basic: GraphNode | null,
  selectedId: string,
): GraphNode {
  const entity = raw.entity
  const canonicalType = mapNodeType(entity.entityType)
  const rawType = entity.entityType.toLowerCase()
  const resolvedSubType =
    entity.entitySubType ?? basic?.subType ?? (rawType !== canonicalType ? rawType : undefined)

  const enriched: GraphNode = {
    id: entity.entityId ?? selectedId,
    label: entity.canonicalLabel ?? basic?.label ?? '',
    type: canonicalType,
    // Keep the topology-derived degree (from `basic`) as the connectivity
    // metric; `supportCount` is surfaced separately in `properties` below.
    edgeCount: basic?.edgeCount ?? entity.supportCount ?? 0,
    properties: {},
    sourceDocumentIds: [],
  }
  if (resolvedSubType !== undefined) enriched.subType = resolvedSubType
  const summary = entity.summary ?? basic?.summary
  if (summary !== undefined) enriched.summary = summary

  if (entity.entityType) enriched.properties['type'] = entity.entityType
  if (entity.confidence != null) {
    enriched.properties['confidence'] = String(Math.round(entity.confidence * 100)) + '%'
  }
  if (entity.supportCount != null) {
    enriched.properties['support count'] = String(entity.supportCount)
  }
  if (entity.entityState) enriched.properties['state'] = entity.entityState
  if (entity.aliases?.length) enriched.properties['aliases'] = entity.aliases.join(', ')

  enriched.sourceDocumentIds = Array.from(
    new Set(raw.supportingEvidence.map((evidence) => evidence.documentId)),
  )

  return enriched
}

function mapGraphDocumentSummary(raw: ContentDocumentDetailResponse): string | undefined {
  const summary = raw.head?.document_summary
  return typeof summary === 'string' && summary.trim().length > 0 ? summary : undefined
}

function mapGraphDocumentRevision(raw: ContentDocumentDetailResponse): ContentRevision | undefined {
  return raw.activeRevision ?? undefined
}

export function mapGraphDocumentDetail(
  raw: ContentDocumentDetailResponse,
  basic: GraphNode | null,
  selectedId: string,
): GraphNode {
  const revision = mapGraphDocumentRevision(raw)
  const isWebPage = revision?.content_source_kind === 'web_page'
  const fileNameLabel = typeof raw.fileName === 'string' ? raw.fileName : undefined
  const label =
    (isWebPage ? revision?.source_uri : undefined) ?? fileNameLabel ?? basic?.label ?? selectedId

  const enriched: GraphNode = {
    id: selectedId,
    label,
    type: 'document',
    edgeCount: basic?.edgeCount ?? 0,
    properties: {},
    sourceDocumentIds: [],
  }
  const summary = mapGraphDocumentSummary(raw) ?? basic?.summary
  if (summary !== undefined) enriched.summary = summary

  if (revision?.mime_type) {
    enriched.properties['format'] = revision.mime_type
  }
  if (revision?.byte_size != null) {
    enriched.properties['size'] = `${(revision.byte_size / 1024).toFixed(1)} KB`
  }
  if (revision?.revision_number != null) {
    enriched.properties['revision'] = String(revision.revision_number)
  }
  enriched.properties['state'] = raw.readinessSummary?.readinessKind ?? 'unknown'
  enriched.properties['activity'] = raw.readinessSummary?.activityStatus ?? 'unknown'
  if (raw.readinessSummary?.graphCoverageKind) {
    enriched.properties['graph coverage'] = raw.readinessSummary.graphCoverageKind
  }

  return enriched
}
