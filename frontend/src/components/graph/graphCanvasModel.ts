import { MultiUndirectedGraph } from 'graphology'
import type { GraphEdge, GraphLayoutMode, GraphNode, GraphNodeType } from 'src/models/ui/graph'

export interface GraphCanvasNodeAttributes {
  label: string
  x: number
  y: number
  size: number
  color: string
  borderColor: string
  borderSize: number
  forceLabel?: boolean
  nodeType: GraphNodeType
  supportCount: number
  filteredArtifact: boolean
}

export interface GraphCanvasEdgeAttributes {
  label: string
  size: number
  color: string
  supportCount: number
  filteredArtifact: boolean
}

export const NODE_BORDER_COLOR = '#ffffff'
export const EDGE_COLOR = 'rgba(90, 113, 159, 0.44)'

const MAX_FOCUS_NEIGHBORS = 120
const GOLDEN_ANGLE = Math.PI * (3 - Math.sqrt(5))
const OVERVIEW_LABEL_LIMITS: Record<GraphNodeType, number> = {
  document: 4,
  entity: 6,
  topic: 4,
}

function hashToUnit(value: string): number {
  let hash = 2166136261
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index)
    hash = Math.imul(hash, 16777619)
  }
  return (hash >>> 0) / 4294967295
}

export function fallbackPosition(nodeId: string): { x: number; y: number } {
  return {
    x: hashToUnit(`${nodeId}:fallback:x`) * 2 - 1,
    y: hashToUnit(`${nodeId}:fallback:y`) * 2 - 1,
  }
}

export function buildNodeMap(nodes: GraphNode[]): Map<string, GraphNode> {
  const map = new Map<string, GraphNode>()
  for (const node of nodes) {
    map.set(node.id, node)
  }
  return map
}

export function aggregateGraphEdges(nodes: GraphNode[], edges: GraphEdge[]): GraphEdge[] {
  const nodeMap = buildNodeMap(nodes)
  const edgeMap = new Map<string, GraphEdge>()

  for (const edge of edges) {
    if (edge.source === edge.target) {
      continue
    }
    if (!nodeMap.has(edge.source) || !nodeMap.has(edge.target)) {
      continue
    }

    const key =
      edge.source < edge.target ? `${edge.source}::${edge.target}` : `${edge.target}::${edge.source}`
    const current = edgeMap.get(key)
    if (!current || edge.supportCount > current.supportCount) {
      edgeMap.set(key, edge)
    }
  }

  return [...edgeMap.values()]
}

export function buildDegreeMap(nodes: GraphNode[], edges: GraphEdge[]): Map<string, number> {
  const degreeMap = new Map<string, number>()
  for (const node of nodes) {
    degreeMap.set(node.id, 0)
  }
  for (const edge of edges) {
    degreeMap.set(edge.source, (degreeMap.get(edge.source) ?? 0) + 1)
    degreeMap.set(edge.target, (degreeMap.get(edge.target) ?? 0) + 1)
  }
  return degreeMap
}

export function filterFocusedNodes(
  nodes: GraphNode[],
  edges: GraphEdge[],
  focusedNodeId: string | null,
  degreeMap: Map<string, number>,
): GraphNode[] {
  if (!focusedNodeId) {
    return nodes
  }

  const nodeMap = buildNodeMap(nodes)
  if (!nodeMap.has(focusedNodeId)) {
    return nodes
  }

  const selected = new Set<string>([focusedNodeId])
  const neighborWeights = new Map<string, number>()

  for (const edge of edges) {
    if (edge.source === focusedNodeId) {
      neighborWeights.set(edge.target, Math.max(neighborWeights.get(edge.target) ?? 0, edge.supportCount))
    } else if (edge.target === focusedNodeId) {
      neighborWeights.set(edge.source, Math.max(neighborWeights.get(edge.source) ?? 0, edge.supportCount))
    }
  }

  const orderedNeighbors = [...neighborWeights.keys()].sort((left, right) => {
    const leftWeight = (neighborWeights.get(left) ?? 0) * 3 + (degreeMap.get(left) ?? 0)
    const rightWeight = (neighborWeights.get(right) ?? 0) * 3 + (degreeMap.get(right) ?? 0)
    return rightWeight - leftWeight
  })

  orderedNeighbors.slice(0, MAX_FOCUS_NEIGHBORS).forEach((nodeId) => selected.add(nodeId))
  return nodes.filter((node) => selected.has(node.id))
}

function nodeColor(nodeType: GraphNodeType): string {
  if (nodeType === 'document') {
    return '#5b55f7'
  }
  if (nodeType === 'entity') {
    return '#f59e0b'
  }
  return '#10b981'
}

function filteredNodeColor(nodeType: GraphNodeType): string {
  if (nodeType === 'document') {
    return 'rgba(91, 85, 247, 0.38)'
  }
  if (nodeType === 'entity') {
    return 'rgba(245, 158, 11, 0.42)'
  }
  return 'rgba(16, 185, 129, 0.4)'
}

function nodeSize(node: GraphNode, degree: number, compact = false): number {
  const influence = Math.max(1, node.supportCount) + degree * 1.6
  const baseSize = compact ? 4.3 : 5.4
  const maxSize = compact ? 10.4 : 14.8
  const scale = compact ? 0.82 : 1.08
  return Math.max(baseSize, Math.min(maxSize, baseSize + Math.sqrt(influence) * scale))
}

function edgeSize(edge: GraphEdge): number {
  return Math.max(1.1, Math.min(3.6, 0.92 + Math.sqrt(Math.max(1, edge.supportCount)) * 0.34))
}

function labelForRelation(edge: GraphEdge): string {
  return edge.relationType.replaceAll('_', ' ')
}

function sortNodesByWeight(nodes: GraphNode[], degreeMap: Map<string, number>): GraphNode[] {
  return [...nodes].sort((left, right) => {
    const leftWeight = left.supportCount * 2 + (degreeMap.get(left.id) ?? 0)
    const rightWeight = right.supportCount * 2 + (degreeMap.get(right.id) ?? 0)
    return rightWeight - leftWeight
  })
}

function selectLabelNodeIds(
  nodes: GraphNode[],
  degreeMap: Map<string, number>,
  focusedNodeId: string | null,
): Set<string> {
  if (focusedNodeId) {
    const selected = new Set<string>([focusedNodeId])
    sortNodesByWeight(nodes, degreeMap)
      .slice(0, 18)
      .forEach((node) => selected.add(node.id))
    return selected
  }

  if (nodes.length <= 36) {
    return new Set(nodes.map((node) => node.id))
  }

  if (nodes.length > 180) {
    return new Set<string>()
  }

  const selected = new Set<string>()
  const groups: Record<GraphNodeType, GraphNode[]> = {
    document: [],
    entity: [],
    topic: [],
  }

  for (const node of nodes) {
    groups[node.nodeType].push(node)
  }

  ;(['document', 'entity', 'topic'] as const).forEach((nodeType) => {
    sortNodesByWeight(groups[nodeType], degreeMap)
      .slice(0, OVERVIEW_LABEL_LIMITS[nodeType])
      .forEach((node) => selected.add(node.id))
  })

  return selected
}

function assignLaneLayout(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  nodes: GraphNode[],
  degreeMap: Map<string, number>,
): void {
  const groups: Record<GraphNodeType, GraphNode[]> = {
    document: [],
    entity: [],
    topic: [],
  }

  for (const node of nodes) {
    groups[node.nodeType].push(node)
  }

  const layouts: Record<
    GraphNodeType,
    { centerX: number; rows: number; xStep: number; yStep: number }
  > = {
    document: { centerX: -1.06, rows: 13, xStep: 0.24, yStep: 0.26 },
    entity: { centerX: 0, rows: 16, xStep: 0.22, yStep: 0.23 },
    topic: { centerX: 1.06, rows: 13, xStep: 0.24, yStep: 0.26 },
  }

  ;(['document', 'entity', 'topic'] as const).forEach((nodeType) => {
    const ordered = sortNodesByWeight(groups[nodeType], degreeMap)
    const { centerX, rows, xStep, yStep } = layouts[nodeType]
    const cols = Math.max(1, Math.ceil(ordered.length / rows))
    ordered.forEach((node, index) => {
      const col = Math.floor(index / rows)
      const row = index % rows
      const columnCount = Math.min(rows, ordered.length - col * rows)
      const offsetX = col * xStep - ((cols - 1) * xStep) / 2
      const rowOffset = row * yStep - ((columnCount - 1) * yStep) / 2
      const jitterX = (hashToUnit(`${node.id}:lane:x`) - 0.5) * 0.05
      const jitterY = (hashToUnit(`${node.id}:lane:y`) - 0.5) * 0.045

      graph.mergeNodeAttributes(node.id, {
        x: centerX + offsetX + jitterX,
        y: rowOffset + jitterY,
      })
    })
  })
}

function assignCloudLayout(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  nodes: GraphNode[],
  degreeMap: Map<string, number>,
): void {
  const groups: Record<GraphNodeType, GraphNode[]> = {
    document: [],
    entity: [],
    topic: [],
  }

  for (const node of nodes) {
    groups[node.nodeType].push(node)
  }

  const clusterAnchors: Record<GraphNodeType, { x: number; y: number }[]> = {
    document: [
      { x: -0.86, y: -0.24 },
      { x: -0.6, y: 0.02 },
      { x: -0.8, y: 0.28 },
    ],
    entity: [
      { x: -0.02, y: -0.12 },
      { x: 0.12, y: 0.08 },
      { x: -0.08, y: 0.3 },
    ],
    topic: [
      { x: 0.74, y: -0.2 },
      { x: 0.92, y: 0.04 },
      { x: 0.7, y: 0.28 },
    ],
  }

  const clusterShape: Record<
    GraphNodeType,
    { radiusBase: number; radiusStep: number; stretchX: number; stretchY: number }
  > = {
    document: { radiusBase: 0.11, radiusStep: 0.085, stretchX: 1.08, stretchY: 0.84 },
    entity: { radiusBase: 0.1, radiusStep: 0.082, stretchX: 1.04, stretchY: 0.9 },
    topic: { radiusBase: 0.1, radiusStep: 0.088, stretchX: 1.02, stretchY: 0.86 },
  }

  ;(['document', 'entity', 'topic'] as const).forEach((nodeType) => {
    const ordered = sortNodesByWeight(groups[nodeType], degreeMap)
    const anchors = clusterAnchors[nodeType]
    const cluster = clusterShape[nodeType]
    ordered.forEach((node, index) => {
      const anchor = anchors[index % anchors.length]
      const localIndex = Math.floor(index / anchors.length)
      const jitter = hashToUnit(`${node.id}:cloud`) - 0.5
      const angle = localIndex * GOLDEN_ANGLE + jitter * 0.34
      const radius =
        cluster.radiusBase +
        Math.sqrt(localIndex + 1) * cluster.radiusStep -
        Math.min(0.11, Math.log10(Math.max(1, node.supportCount)) * 0.03)

      graph.mergeNodeAttributes(node.id, {
        x: anchor.x + Math.cos(angle) * radius * cluster.stretchX,
        y: anchor.y + Math.sin(angle) * radius * cluster.stretchY,
      })
    })
  })
}

function assignRingLayout(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  nodes: GraphNode[],
  degreeMap: Map<string, number>,
): void {
  const groups: Record<GraphNodeType, GraphNode[]> = {
    document: [],
    entity: [],
    topic: [],
  }

  for (const node of nodes) {
    groups[node.nodeType].push(node)
  }

  const radii: Record<GraphNodeType, { base: number; gap: number; perRing: number }> = {
    document: { base: 0.48, gap: 0.3, perRing: 16 },
    entity: { base: 1.18, gap: 0.24, perRing: 24 },
    topic: { base: 1.94, gap: 0.28, perRing: 18 },
  }

  ;(['document', 'entity', 'topic'] as const).forEach((nodeType) => {
    const ordered = sortNodesByWeight(groups[nodeType], degreeMap)
    const { base, gap, perRing } = radii[nodeType]

    ordered.forEach((node, index) => {
      const ringIndex = Math.floor(index / perRing)
      const positionInRing = index % perRing
      const ringCount = Math.min(perRing, ordered.length - ringIndex * perRing)
      const angle =
        (positionInRing / Math.max(1, ringCount)) * Math.PI * 2 +
        (nodeType === 'entity' ? Math.PI / 14 : nodeType === 'topic' ? Math.PI / 8 : 0)
      const wobble = (hashToUnit(`${node.id}:ring`) - 0.5) * 0.06
      const radius = base + ringIndex * gap + wobble
      graph.mergeNodeAttributes(node.id, {
        x: Math.cos(angle) * radius * 1.08,
        y: Math.sin(angle) * radius * 0.92,
      })
    })
  })
}

export function ensureFinitePositions(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
): void {
  graph.forEachNode((nodeId) => {
    const attributes = graph.getNodeAttributes(nodeId)
    if (Number.isFinite(attributes.x) && Number.isFinite(attributes.y)) {
      return
    }
    const fallback = fallbackPosition(nodeId)
    graph.setNodeAttribute(nodeId, 'x', fallback.x)
    graph.setNodeAttribute(nodeId, 'y', fallback.y)
  })
}

export function normalizeGraphBounds(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
): void {
  let minX = Number.POSITIVE_INFINITY
  let maxX = Number.NEGATIVE_INFINITY
  let minY = Number.POSITIVE_INFINITY
  let maxY = Number.NEGATIVE_INFINITY

  graph.forEachNode((nodeId) => {
    const attributes = graph.getNodeAttributes(nodeId)
    minX = Math.min(minX, attributes.x)
    maxX = Math.max(maxX, attributes.x)
    minY = Math.min(minY, attributes.y)
    maxY = Math.max(maxY, attributes.y)
  })

  if (
    !Number.isFinite(minX) ||
    !Number.isFinite(maxX) ||
    !Number.isFinite(minY) ||
    !Number.isFinite(maxY)
  ) {
    return
  }

  const width = Math.max(0.001, maxX - minX)
  const height = Math.max(0.001, maxY - minY)
  const scale = 2 / Math.max(width, height)
  const centerX = (minX + maxX) / 2
  const centerY = (minY + maxY) / 2

  graph.forEachNode((nodeId) => {
    const attributes = graph.getNodeAttributes(nodeId)
    graph.mergeNodeAttributes(nodeId, {
      x: (attributes.x - centerX) * scale,
      y: (attributes.y - centerY) * scale,
    })
  })
}

function applyLayout(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  nodes: GraphNode[],
  edges: GraphEdge[],
  layoutMode: GraphLayoutMode,
): void {
  const degreeMap = buildDegreeMap(nodes, edges)

  if (layoutMode === 'lanes') {
    assignLaneLayout(graph, nodes, degreeMap)
  } else if (layoutMode === 'rings') {
    assignRingLayout(graph, nodes, degreeMap)
  } else {
    assignCloudLayout(graph, nodes, degreeMap)
  }

  ensureFinitePositions(graph)
  normalizeGraphBounds(graph)
}

export function createGraphModel(
  nodes: GraphNode[],
  edges: GraphEdge[],
  focusedNodeId: string | null,
  layoutMode: GraphLayoutMode,
): MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes> {
  const graph = new MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>()
  const degreeMap = buildDegreeMap(nodes, edges)
  const compactLabels = !focusedNodeId && nodes.length > 72
  const labelNodeIds = selectLabelNodeIds(nodes, degreeMap, focusedNodeId)

  for (const node of nodes) {
    graph.addNode(node.id, {
      label: node.label,
      x: 0,
      y: 0,
      size: nodeSize(node, degreeMap.get(node.id) ?? 0, compactLabels),
      color: node.filteredArtifact ? filteredNodeColor(node.nodeType) : nodeColor(node.nodeType),
      borderColor: node.filteredArtifact ? 'rgba(148, 163, 184, 0.72)' : NODE_BORDER_COLOR,
      borderSize: node.filteredArtifact ? 0.12 : 0.18,
      nodeType: node.nodeType,
      supportCount: node.supportCount,
      forceLabel: labelNodeIds.has(node.id),
      filteredArtifact: node.filteredArtifact,
    })
  }

  for (const edge of edges) {
    if (!graph.hasNode(edge.source) || !graph.hasNode(edge.target)) {
      continue
    }
    graph.addEdge(edge.source, edge.target, {
      label: labelForRelation(edge),
      size: edge.filteredArtifact ? Math.max(1.1, edgeSize(edge) * 0.72) : edgeSize(edge),
      color: edge.filteredArtifact ? 'rgba(148, 163, 184, 0.42)' : EDGE_COLOR,
      supportCount: edge.supportCount,
      filteredArtifact: edge.filteredArtifact,
    })
  }

  applyLayout(graph, nodes, edges, layoutMode)
  return graph
}

export function relayoutGraphModel(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  nodes: GraphNode[],
  edges: GraphEdge[],
  layoutMode: GraphLayoutMode,
): void {
  applyLayout(graph, nodes, edges, layoutMode)
}
