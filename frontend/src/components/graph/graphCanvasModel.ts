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
export const EDGE_COLOR = 'rgba(69, 91, 136, 0.7)'
export const FILTERED_EDGE_COLOR = 'rgba(244, 63, 94, 0.48)'

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
    return 'rgba(91, 85, 247, 0.24)'
  }
  if (nodeType === 'entity') {
    return 'rgba(245, 158, 11, 0.26)'
  }
  return 'rgba(16, 185, 129, 0.24)'
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

function buildAdjacencyMap(nodes: GraphNode[], edges: GraphEdge[]): Map<string, string[]> {
  const adjacency = new Map<string, Set<string>>()
  for (const node of nodes) {
    adjacency.set(node.id, new Set())
  }

  for (const edge of edges) {
    adjacency.get(edge.source)?.add(edge.target)
    adjacency.get(edge.target)?.add(edge.source)
  }

  return new Map(
    [...adjacency.entries()].map(([nodeId, neighbors]) => [nodeId, [...neighbors.values()]]),
  )
}

function assignRadialSet(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  nodeIds: string[],
  centerX: number,
  centerY: number,
  startRadius: number,
  radiusStep: number,
  perRing: number,
  jitterKey: string,
  stretchX = 1,
  stretchY = 1,
): void {
  nodeIds.forEach((nodeId, index) => {
    const ringIndex = Math.floor(index / perRing)
    const positionInRing = index % perRing
    const ringCount = Math.min(perRing, nodeIds.length - ringIndex * perRing)
    const angle = (positionInRing / Math.max(1, ringCount)) * Math.PI * 2
    const jitter = (hashToUnit(`${nodeId}:${jitterKey}`) - 0.5) * 0.06
    const radius = startRadius + ringIndex * radiusStep + jitter
    graph.mergeNodeAttributes(nodeId, {
      x: centerX + Math.cos(angle) * radius * stretchX,
      y: centerY + Math.sin(angle) * radius * stretchY,
    })
  })
}

function clusterCenters(
  count: number,
  radiusBase: number,
  radiusStep: number,
  stretchX = 1,
  stretchY = 1,
): { x: number; y: number }[] {
  return Array.from({ length: count }, (_, index) => {
    const angle = index * GOLDEN_ANGLE
    const radius = radiusBase + Math.sqrt(index) * radiusStep
    return {
      x: Math.cos(angle) * radius * stretchX,
      y: Math.sin(angle) * radius * stretchY,
    }
  })
}

function buildSeededClusterEntries(
  nodes: GraphNode[],
  edges: GraphEdge[],
  degreeMap: Map<string, number>,
): [string, GraphNode[]][] {
  const adjacency = buildAdjacencyMap(nodes, edges)
  const ordered = sortNodesByWeight(nodes, degreeMap)
  const desiredSeedCount = Math.min(8, Math.max(3, Math.round(Math.sqrt(nodes.length) / 3)))
  const seeds: string[] = []

  for (const node of ordered) {
    if (seeds.every((seedId) => !(adjacency.get(seedId) ?? []).includes(node.id))) {
      seeds.push(node.id)
    }
    if (seeds.length >= desiredSeedCount) {
      break
    }
  }

  if (!seeds.length && ordered[0]) {
    seeds.push(ordered[0].id)
  }

  const assignments = new Map<string, string>()
  const queue: string[] = []

  for (const seedId of seeds) {
    assignments.set(seedId, seedId)
    queue.push(seedId)
  }

  while (queue.length) {
    const current = queue.shift()
    if (!current) {
      continue
    }
    const clusterId = assignments.get(current)
    if (!clusterId) {
      continue
    }
    for (const neighbor of adjacency.get(current) ?? []) {
      if (assignments.has(neighbor)) {
        continue
      }
      assignments.set(neighbor, clusterId)
      queue.push(neighbor)
    }
  }

  for (const node of nodes) {
    if (assignments.has(node.id)) {
      continue
    }

    const neighborClusters = new Map<string, number>()
    for (const neighbor of adjacency.get(node.id) ?? []) {
      const clusterId = assignments.get(neighbor)
      if (!clusterId) {
        continue
      }
      neighborClusters.set(clusterId, (neighborClusters.get(clusterId) ?? 0) + 1)
    }

    const clusterId =
      [...neighborClusters.entries()].sort((left, right) => right[1] - left[1])[0]?.[0] ??
      seeds[node.id.length % Math.max(1, seeds.length)]

    assignments.set(node.id, clusterId)
  }

  const clusters = new Map<string, GraphNode[]>()
  for (const node of nodes) {
    const clusterId = assignments.get(node.id) ?? node.id
    const clusterNodes = clusters.get(clusterId)
    if (clusterNodes) {
      clusterNodes.push(node)
    } else {
      clusters.set(clusterId, [node])
    }
  }

  return [...clusters.entries()].sort(
    (left, right) =>
      right[1].reduce((total, node) => total + node.supportCount + (degreeMap.get(node.id) ?? 0), 0) -
      left[1].reduce((total, node) => total + node.supportCount + (degreeMap.get(node.id) ?? 0), 0),
  )
}

function archipelagoCenters(count: number): { x: number; y: number }[] {
  const centers: { x: number; y: number }[] = []
  let placed = 0
  let ringIndex = 0

  while (placed < count) {
    const ringCapacity = ringIndex === 0 ? Math.min(6, count) : Math.min(count - placed, 8 + ringIndex * 4)
    const radius = 1.18 + ringIndex * 0.82
    for (let index = 0; index < ringCapacity; index += 1) {
      const angle = (index / Math.max(1, ringCapacity)) * Math.PI * 2 + ringIndex * 0.34
      centers.push({
        x: Math.cos(angle) * radius * 1.26,
        y: Math.sin(angle) * radius * 0.94,
      })
    }
    placed += ringCapacity
    ringIndex += 1
  }

  return centers
}

function assignClusterLayout(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  nodes: GraphNode[],
  edges: GraphEdge[],
  degreeMap: Map<string, number>,
): void {
  const clusterEntries = buildSeededClusterEntries(nodes, edges, degreeMap)
  const centers = clusterCenters(clusterEntries.length, 0.22, 0.5, 1.18, 0.9)

  clusterEntries.forEach(([clusterId, clusterNodes], index) => {
    const center = centers[index] ?? { x: 0, y: 0 }
    const orderedCluster = sortNodesByWeight(clusterNodes, degreeMap)
    const seedIndex = Math.max(
      0,
      orderedCluster.findIndex((node) => node.id === clusterId),
    )
    const seedNode = orderedCluster[seedIndex] ?? orderedCluster[0]
    const rest = orderedCluster.filter((node) => node.id !== seedNode.id)

    graph.mergeNodeAttributes(seedNode.id, {
      x: center.x,
      y: center.y,
    })
    assignRadialSet(
      graph,
      rest.map((node) => node.id),
      center.x,
      center.y,
      0.18,
      0.13,
      10,
      'cluster',
      1.04,
      0.88,
    )
  })
}

function assignIslandLayout(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  nodes: GraphNode[],
  edges: GraphEdge[],
  degreeMap: Map<string, number>,
): void {
  const islandEntries = buildSeededClusterEntries(nodes, edges, degreeMap)
  const centers = archipelagoCenters(islandEntries.length)

  islandEntries.forEach(([clusterId, clusterNodes], index) => {
    const center = centers[index] ?? { x: 0, y: 0 }
    const ordered = sortNodesByWeight(clusterNodes, degreeMap)
    const seedIndex = Math.max(
      0,
      ordered.findIndex((node) => node.id === clusterId),
    )
    const seedNode = ordered[seedIndex] ?? ordered[0]
    const rest = ordered.filter((node) => node.id !== seedNode.id)

    graph.mergeNodeAttributes(seedNode.id, {
      x: center.x,
      y: center.y,
    })
    assignRadialSet(
      graph,
      rest.map((node) => node.id),
      center.x,
      center.y,
      0.12,
      0.085,
      12,
      'island',
      0.98,
      0.84,
    )
  })
}

function assignSpiralLayout(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  nodes: GraphNode[],
  degreeMap: Map<string, number>,
): void {
  const ordered = sortNodesByWeight(nodes, degreeMap)
  ordered.forEach((node, index) => {
    const step = index * 0.33
    const radius = 0.08 + step * 0.06
    const angle = step + (node.nodeType === 'entity' ? 0.22 : node.nodeType === 'topic' ? 0.44 : 0)
    const jitter = (hashToUnit(`${node.id}:spiral`) - 0.5) * 0.04
    graph.mergeNodeAttributes(node.id, {
      x: Math.cos(angle) * (radius + jitter) * 1.06,
      y: Math.sin(angle) * (radius + jitter) * 0.9,
    })
  })
}

function assignCircleLayout(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  nodes: GraphNode[],
  degreeMap: Map<string, number>,
): void {
  const ordered = sortNodesByWeight(nodes, degreeMap)
  const baseCapacity = 64
  const ringGap = 0.12
  const innerRadius = 1.36
  let startIndex = 0
  let ringIndex = 0

  while (startIndex < ordered.length) {
    const ringCapacity = baseCapacity + ringIndex * 24
    const ringNodes = ordered.slice(startIndex, startIndex + ringCapacity)
    const radius = innerRadius + ringIndex * ringGap
    const angularOffset = ringIndex * 0.22

    ringNodes.forEach((node, index) => {
      const t = index / Math.max(1, ringNodes.length)
      const typeBias =
        node.nodeType === 'document' ? 0 : node.nodeType === 'entity' ? Math.PI / 20 : Math.PI / 10
      const jitter = (hashToUnit(`${node.id}:circle`) - 0.5) * 0.035
      const angle = t * Math.PI * 2 + angularOffset + typeBias + jitter
      const localRadius =
        radius +
        (hashToUnit(`${node.id}:circle:radius`) - 0.5) * 0.04 -
        Math.min(0.06, Math.log10(Math.max(1, node.supportCount)) * 0.012)

      graph.mergeNodeAttributes(node.id, {
        x: Math.cos(angle) * localRadius * 1.1,
        y: Math.sin(angle) * localRadius * 0.94,
      })
    })

    startIndex += ringCapacity
    ringIndex += 1
  }
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
  } else if (layoutMode === 'circle') {
    assignCircleLayout(graph, nodes, degreeMap)
  } else if (layoutMode === 'clusters') {
    assignClusterLayout(graph, nodes, edges, degreeMap)
  } else if (layoutMode === 'islands') {
    assignIslandLayout(graph, nodes, edges, degreeMap)
  } else if (layoutMode === 'spiral') {
    assignSpiralLayout(graph, nodes, degreeMap)
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
      borderColor: node.filteredArtifact ? 'rgba(244, 63, 94, 0.9)' : NODE_BORDER_COLOR,
      borderSize: node.filteredArtifact ? 0.32 : 0.18,
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
      color: edge.filteredArtifact ? FILTERED_EDGE_COLOR : EDGE_COLOR,
      supportCount: edge.supportCount,
      filteredArtifact: edge.filteredArtifact,
    })
  }

  applyLayout(graph, nodes, edges, layoutMode)
  return graph
}
