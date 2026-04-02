import { MultiUndirectedGraph } from 'graphology'
import type {
  GraphCanvasMode,
  GraphEdge,
  GraphInspectorState,
  GraphLayoutMode,
  GraphNode,
  GraphNodeDetail,
  GraphNodeType,
  GraphOverlayState,
  GraphSearchHit,
  GraphStatus,
} from 'src/models/ui/graph'
import { resolveDefaultGraphLayoutMode } from 'src/models/ui/graph'

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
  focusRole?: 'focus' | 'neighbor'
}

export interface GraphCanvasEdgeAttributes {
  edgeId: string
  label: string
  size: number
  color: string
  supportCount: number
  filteredArtifact: boolean
  focusEdge?: boolean
}

export const NODE_BORDER_COLOR = '#ffffff'
export const EDGE_COLOR = 'rgba(69, 91, 136, 0.7)'
export const DENSE_EDGE_COLOR = 'rgba(69, 91, 136, 0.58)'
export const FILTERED_EDGE_COLOR = 'rgba(244, 63, 94, 0.48)'
export const FOCUS_EDGE_COLOR = 'rgba(29, 78, 216, 0.96)'
export const FOCUS_NODE_BORDER_COLOR = 'rgba(15, 23, 42, 0.96)'
export const NEIGHBOR_NODE_BORDER_COLOR = 'rgba(255, 255, 255, 0.96)'

const GOLDEN_ANGLE = Math.PI * (3 - Math.sqrt(5))
const LARGE_OVERVIEW_NODE_THRESHOLD = 240
const MIN_NORMALIZED_LAYOUT_EXTENT = 0.88
const NORMALIZED_LAYOUT_PADDING = 0.16
const FOCUS_NEIGHBOR_LABEL_LIMIT = 8
const DENSE_FOCUS_NEIGHBOR_LABEL_LIMIT = 4
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

function selectOverviewLabelNodeIds(
  nodes: GraphNode[],
  degreeMap: Map<string, number>,
): Set<string> {
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

interface GraphVisualContext {
  degreeMap: Map<string, number>
  compactLabels: boolean
  denseGraph: boolean
  labelNodeIds: Set<string>
  focusedNodeId: string | null
  focusedNeighborIds: Set<string>
  focusedEdgeIds: Set<string>
}

function createGraphVisualContext(
  nodes: GraphNode[],
  edges: GraphEdge[],
  degreeMap?: Map<string, number>,
  focusedNodeId: string | null = null,
): GraphVisualContext {
  const resolvedDegreeMap = degreeMap ?? buildDegreeMap(nodes, edges)
  const compactLabels = nodes.length > 72
  const denseGraph = nodes.length > LARGE_OVERVIEW_NODE_THRESHOLD
  const focusedNeighborIds = new Set<string>()
  const focusedEdgeIds = new Set<string>()

  if (focusedNodeId) {
    for (const edge of edges) {
      if (edge.source === focusedNodeId) {
        focusedNeighborIds.add(edge.target)
        focusedEdgeIds.add(edge.id)
      } else if (edge.target === focusedNodeId) {
        focusedNeighborIds.add(edge.source)
        focusedEdgeIds.add(edge.id)
      }
    }
  }

  const labelNodeIds = focusedNodeId
    ? new Set<string>([focusedNodeId])
    : selectOverviewLabelNodeIds(nodes, resolvedDegreeMap)

  if (focusedNodeId && focusedNeighborIds.size > 0) {
    const neighborLabelLimit = denseGraph
      ? DENSE_FOCUS_NEIGHBOR_LABEL_LIMIT
      : FOCUS_NEIGHBOR_LABEL_LIMIT
    const prioritizedNeighbors = sortNodesByWeight(
      nodes.filter((node) => focusedNeighborIds.has(node.id)),
      resolvedDegreeMap,
    ).slice(0, neighborLabelLimit)

    prioritizedNeighbors.forEach((node) => labelNodeIds.add(node.id))
  }

  return {
    degreeMap: resolvedDegreeMap,
    compactLabels,
    denseGraph,
    labelNodeIds,
    focusedNodeId,
    focusedNeighborIds,
    focusedEdgeIds,
  }
}

type GraphNodeVisualAttributes = Omit<GraphCanvasNodeAttributes, 'x' | 'y'>

function resolveNodeVisualAttributes(
  node: GraphNode,
  context: GraphVisualContext,
): GraphNodeVisualAttributes {
  const baseSize = nodeSize(node, context.degreeMap.get(node.id) ?? 0, context.compactLabels)
  const overviewSize = context.denseGraph ? Math.max(2.4, baseSize * 0.66) : baseSize
  const color = node.filteredArtifact ? filteredNodeColor(node.nodeType) : nodeColor(node.nodeType)
  let borderColor = node.filteredArtifact ? 'rgba(244, 63, 94, 0.9)' : NODE_BORDER_COLOR
  let borderSize = node.filteredArtifact ? 0.32 : context.denseGraph ? 0.1 : 0.18
  let size = overviewSize
  let focusRole: GraphCanvasNodeAttributes['focusRole']
  let forceLabel = context.labelNodeIds.has(node.id)

  if (context.focusedNodeId === node.id) {
    size = Math.max(size + 8.6, size * 2.04)
    borderColor = FOCUS_NODE_BORDER_COLOR
    borderSize = 1.28
    focusRole = 'focus'
    forceLabel = true
  } else if (context.focusedNeighborIds.has(node.id)) {
    size = Math.max(size + 3.1, size * 1.24)
    borderColor = NEIGHBOR_NODE_BORDER_COLOR
    borderSize = 0.52
    focusRole = 'neighbor'
  }

  return {
    label: node.label,
    size,
    color,
    borderColor,
    borderSize,
    nodeType: node.nodeType,
    supportCount: node.supportCount,
    forceLabel,
    filteredArtifact: node.filteredArtifact,
    focusRole,
  }
}

function resolveEdgeVisualAttributes(
  edge: GraphEdge,
  context: GraphVisualContext,
): GraphCanvasEdgeAttributes {
  const baseSize = edge.filteredArtifact
    ? Math.max(1.1, edgeSize(edge) * 0.72)
    : context.denseGraph
      ? Math.max(0.72, edgeSize(edge) * 0.74)
      : edgeSize(edge)
  const baseColor = edge.filteredArtifact
    ? FILTERED_EDGE_COLOR
    : context.denseGraph
      ? DENSE_EDGE_COLOR
      : EDGE_COLOR
  const focusEdge = context.focusedEdgeIds.has(edge.id)
  const size = focusEdge ? Math.max(2.4, baseSize * 1.9) : baseSize
  const color = focusEdge ? FOCUS_EDGE_COLOR : baseColor

  return {
    edgeId: edge.id,
    label: labelForRelation(edge),
    size,
    color,
    supportCount: edge.supportCount,
    filteredArtifact: edge.filteredArtifact,
    focusEdge,
  }
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

function assignPackedSet(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  nodeIds: string[],
  centerX: number,
  centerY: number,
  radiusBase: number,
  radiusStep: number,
  jitterKey: string,
  stretchX = 1,
  stretchY = 1,
  angleOffset = 0,
): void {
  nodeIds.forEach((nodeId, index) => {
    const localIndex = index + 1
    const angleJitter = (hashToUnit(`${nodeId}:${jitterKey}:angle`) - 0.5) * 0.72
    const radiusJitter = (hashToUnit(`${nodeId}:${jitterKey}:radius`) - 0.5) * radiusStep * 0.92
    const driftX = (hashToUnit(`${nodeId}:${jitterKey}:drift:x`) - 0.5) * radiusStep * 0.96
    const driftY = (hashToUnit(`${nodeId}:${jitterKey}:drift:y`) - 0.5) * radiusStep * 0.78
    const radius = radiusBase + Math.sqrt(localIndex) * radiusStep + radiusJitter
    const angle = localIndex * GOLDEN_ANGLE + angleOffset + angleJitter
    graph.mergeNodeAttributes(nodeId, {
      x: centerX + Math.cos(angle) * radius * stretchX + driftX,
      y: centerY + Math.sin(angle) * radius * stretchY + driftY,
    })
  })
}

function resolveDirectionalAngle(
  origin: { x: number; y: number },
  reference: { x: number; y: number },
  fallbackAngle: number,
): number {
  const dx = origin.x - reference.x
  const dy = origin.y - reference.y
  if (Math.abs(dx) < 0.0001 && Math.abs(dy) < 0.0001) {
    return fallbackAngle
  }
  return Math.atan2(dy, dx)
}

function assignLeafFanSet(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  leafNodeIds: string[],
  origin: { x: number; y: number },
  reference: { x: number; y: number },
  radiusBase: number,
  radiusStep: number,
  jitterKey: string,
  fallbackAngle: number,
): void {
  if (leafNodeIds.length === 0) {
    return
  }

  const baseAngle = resolveDirectionalAngle(origin, reference, fallbackAngle)
  const ringCapacity = Math.min(8, Math.max(4, Math.ceil(Math.sqrt(leafNodeIds.length) * 2)))
  const arcSpread = Math.min(Math.PI * 0.8, 0.42 + ringCapacity * 0.12)

  leafNodeIds.forEach((nodeId, index) => {
    const ringIndex = Math.floor(index / ringCapacity)
    const positionInRing = index % ringCapacity
    const itemsInRing = Math.min(ringCapacity, leafNodeIds.length - ringIndex * ringCapacity)
    const angleOffset =
      itemsInRing <= 1 ? 0 : (positionInRing / Math.max(1, itemsInRing - 1) - 0.5) * arcSpread
    const angleJitter = (hashToUnit(`${nodeId}:${jitterKey}:angle`) - 0.5) * 0.16
    const radiusJitter = (hashToUnit(`${nodeId}:${jitterKey}:radius`) - 0.5) * radiusStep * 0.28
    const driftX = (hashToUnit(`${nodeId}:${jitterKey}:drift:x`) - 0.5) * radiusStep * 0.3
    const driftY = (hashToUnit(`${nodeId}:${jitterKey}:drift:y`) - 0.5) * radiusStep * 0.24
    const radius = radiusBase + ringIndex * radiusStep + radiusJitter
    const angle = baseAngle + angleOffset + angleJitter

    graph.mergeNodeAttributes(nodeId, {
      x: origin.x + Math.cos(angle) * radius * 1.04 + driftX,
      y: origin.y + Math.sin(angle) * radius * 0.92 + driftY,
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
  const desiredSeedCount = Math.min(28, Math.max(6, Math.round(Math.sqrt(nodes.length) / 1.6)))
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
      right[1].reduce(
        (total, node) => total + node.supportCount + (degreeMap.get(node.id) ?? 0),
        0,
      ) -
      left[1].reduce((total, node) => total + node.supportCount + (degreeMap.get(node.id) ?? 0), 0),
  )
}

function resolveClusterAnchorCount(clusterSize: number): number {
  if (clusterSize <= 18) {
    return 1
  }
  if (clusterSize <= 42) {
    return 2
  }
  if (clusterSize <= 120) {
    return Math.min(6, Math.max(3, Math.round(Math.sqrt(clusterSize) / 2.5)))
  }
  return Math.min(8, Math.max(4, Math.round(Math.sqrt(clusterSize) / 2.18)))
}

function selectClusterAnchorNodes(orderedCluster: GraphNode[], clusterId: string): GraphNode[] {
  if (!orderedCluster.length) {
    return []
  }

  const desiredAnchorCount = Math.min(
    resolveClusterAnchorCount(orderedCluster.length),
    orderedCluster.length,
  )
  const seedNode = orderedCluster.find((node) => node.id === clusterId) ?? orderedCluster[0]

  if (desiredAnchorCount === 1) {
    return [seedNode]
  }

  const anchorPool = [seedNode, ...orderedCluster.filter((node) => node.id !== seedNode.id)]
  const selectionWindow = Math.min(
    anchorPool.length,
    Math.max(desiredAnchorCount * 6, desiredAnchorCount),
  )
  const anchors: GraphNode[] = []
  const selectedIds = new Set<string>()

  for (let index = 0; index < desiredAnchorCount; index += 1) {
    const poolIndex =
      index === 0
        ? 0
        : Math.min(
            selectionWindow - 1,
            Math.round((index * (selectionWindow - 1)) / Math.max(1, desiredAnchorCount - 1)),
          )
    const candidate = anchorPool[poolIndex]
    if (!candidate || selectedIds.has(candidate.id)) {
      continue
    }
    anchors.push(candidate)
    selectedIds.add(candidate.id)
  }

  if (anchors.length < desiredAnchorCount) {
    for (const candidate of anchorPool) {
      if (selectedIds.has(candidate.id)) {
        continue
      }
      anchors.push(candidate)
      selectedIds.add(candidate.id)
      if (anchors.length >= desiredAnchorCount) {
        break
      }
    }
  }

  return anchors
}

function resolveClusterAnchorCenters(
  center: { x: number; y: number },
  clusterSize: number,
  anchorCount: number,
  clusterIndex: number,
): Array<{ x: number; y: number }> {
  if (anchorCount <= 1) {
    return [center]
  }

  const radius = Math.min(0.92, 0.24 + Math.sqrt(clusterSize) * 0.05)
  return Array.from({ length: anchorCount }, (_, index) => {
    const angle = (index / anchorCount) * Math.PI * 2 + clusterIndex * 0.27
    return {
      x: center.x + Math.cos(angle) * radius * 1.16,
      y: center.y + Math.sin(angle) * radius * 0.94,
    }
  })
}

function assignClusterConstellation(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  clusterId: string,
  clusterNodes: GraphNode[],
  clusterCenter: { x: number; y: number },
  clusterIndex: number,
  degreeMap: Map<string, number>,
  adjacency: Map<string, string[]>,
): void {
  const orderedCluster = sortNodesByWeight(clusterNodes, degreeMap)
  const anchorNodes = selectClusterAnchorNodes(orderedCluster, clusterId)
  const anchorIds = anchorNodes.map((node) => node.id)
  const anchorCenters = resolveClusterAnchorCenters(
    clusterCenter,
    clusterNodes.length,
    anchorNodes.length,
    clusterIndex,
  )

  anchorNodes.forEach((anchorNode, index) => {
    const anchorCenter = anchorCenters[index] ?? clusterCenter
    graph.mergeNodeAttributes(anchorNode.id, {
      x: anchorCenter.x,
      y: anchorCenter.y,
    })
  })

  const groupedNodeIds = new Map<string, string[]>()
  anchorIds.forEach((anchorId) => groupedNodeIds.set(anchorId, []))
  const anchorLoads = new Map<string, number>()
  anchorIds.forEach((anchorId) => anchorLoads.set(anchorId, 0))
  const targetAnchorLoad = Math.max(
    8,
    Math.ceil(
      Math.max(0, orderedCluster.length - anchorIds.length) / Math.max(1, anchorIds.length),
    ),
  )

  orderedCluster
    .filter((node) => !anchorIds.includes(node.id))
    .forEach((node) => {
      const connections = new Set(adjacency.get(node.id) ?? [])
      const bestAnchorId =
        anchorIds.reduce<{ anchorId: string; score: number } | null>((best, anchorId, index) => {
          const directConnectionScore = connections.has(anchorId) ? 2.4 : 0
          const currentLoad = anchorLoads.get(anchorId) ?? 0
          const overloadPenalty = currentLoad / targetAnchorLoad
          const deterministicBias = hashToUnit(`${node.id}:${anchorId}:cluster-anchor`) * 0.08
          const positionBias = (anchorIds.length - index) * 0.01
          const score = directConnectionScore - overloadPenalty + deterministicBias + positionBias

          if (!best || score > best.score) {
            return { anchorId, score }
          }
          return best
        }, null)?.anchorId ?? anchorIds[0]

      groupedNodeIds.get(bestAnchorId)?.push(node.id)
      anchorLoads.set(bestAnchorId, (anchorLoads.get(bestAnchorId) ?? 0) + 1)
    })

  const clusterSpreadScale = Math.min(2.3, 1.08 + Math.sqrt(clusterNodes.length) / 13)
  anchorIds.forEach((anchorId, index) => {
    const anchorCenter = anchorCenters[index] ?? clusterCenter
    const groupedIds = groupedNodeIds.get(anchorId) ?? []
    const leafNodeIds = groupedIds.filter((nodeId) => (degreeMap.get(nodeId) ?? 0) <= 1)
    const coreNodeIds = groupedIds.filter((nodeId) => (degreeMap.get(nodeId) ?? 0) > 1)

    assignPackedSet(
      graph,
      coreNodeIds,
      anchorCenter.x,
      anchorCenter.y,
      anchorIds.length === 1 ? 0.16 * clusterSpreadScale : 0.12 * clusterSpreadScale,
      anchorIds.length === 1 ? 0.18 * clusterSpreadScale : 0.14 * clusterSpreadScale,
      `cluster:${anchorId}`,
      1.28,
      1.04,
      clusterIndex * 0.24 + index * 0.37,
    )

    const parentLeafGroups = new Map<string, string[]>()
    leafNodeIds.forEach((leafNodeId) => {
      const neighbors = adjacency.get(leafNodeId) ?? []
      const preferredParentId =
        neighbors.find((neighborId) => {
          if (neighborId === anchorId) {
            return true
          }
          return (degreeMap.get(neighborId) ?? 0) > 1
        }) ?? anchorId
      const groupedLeafIds = parentLeafGroups.get(preferredParentId)
      if (groupedLeafIds) {
        groupedLeafIds.push(leafNodeId)
      } else {
        parentLeafGroups.set(preferredParentId, [leafNodeId])
      }
    })

    parentLeafGroups.forEach((leafIds, parentId) => {
      const fallbackAngle =
        clusterIndex * 0.24 + index * 0.37 + (hashToUnit(`${parentId}:leaf-fallback`) - 0.5) * 0.62
      const parentAttributes = graph.hasNode(parentId)
        ? graph.getNodeAttributes(parentId)
        : { x: anchorCenter.x, y: anchorCenter.y }
      const reference =
        parentId === anchorId ? clusterCenter : { x: anchorCenter.x, y: anchorCenter.y }

      assignLeafFanSet(
        graph,
        leafIds,
        { x: parentAttributes.x, y: parentAttributes.y },
        reference,
        0.11 * clusterSpreadScale,
        0.06 * clusterSpreadScale,
        `leaf:${parentId}`,
        fallbackAngle,
      )
    })
  })
}

function archipelagoCenters(count: number): { x: number; y: number }[] {
  const centers: { x: number; y: number }[] = []
  let placed = 0
  let ringIndex = 0

  while (placed < count) {
    const ringCapacity =
      ringIndex === 0 ? Math.min(6, count) : Math.min(count - placed, 8 + ringIndex * 4)
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
  const adjacency = buildAdjacencyMap(nodes, edges)
  const centerRadiusBase = clusterEntries.length > 12 ? 1.24 : 1.12
  const centerRadiusStep = clusterEntries.length > 12 ? 1.36 : 1.24
  const centers = clusterCenters(
    clusterEntries.length,
    centerRadiusBase,
    centerRadiusStep,
    1.84,
    1.32,
  )

  clusterEntries.forEach(([clusterId, clusterNodes], index) => {
    const center = centers[index] ?? { x: 0, y: 0 }
    assignClusterConstellation(graph, clusterId, clusterNodes, center, index, degreeMap, adjacency)
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
    assignPackedSet(
      graph,
      rest.map((node) => node.id),
      center.x,
      center.y,
      0.08,
      0.072,
      'island',
      1,
      0.88,
      index * 0.31,
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
  options?: {
    minExtent?: number
    padding?: number
  },
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

  const minExtent = options?.minExtent ?? MIN_NORMALIZED_LAYOUT_EXTENT
  const padding = options?.padding ?? NORMALIZED_LAYOUT_PADDING
  const width = Math.max(minExtent, maxX - minX)
  const height = Math.max(minExtent, maxY - minY)
  const scale = 2 / Math.max(width + padding, height + padding)
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
  degreeMap?: Map<string, number>,
): void {
  const degrees = degreeMap ?? buildDegreeMap(nodes, edges)

  if (layoutMode === 'lanes') {
    assignLaneLayout(graph, nodes, degrees)
  } else if (layoutMode === 'circle') {
    assignCircleLayout(graph, nodes, degrees)
  } else if (layoutMode === 'clusters') {
    assignClusterLayout(graph, nodes, edges, degrees)
  } else if (layoutMode === 'islands') {
    assignIslandLayout(graph, nodes, edges, degrees)
  } else if (layoutMode === 'spiral') {
    assignSpiralLayout(graph, nodes, degrees)
  } else if (layoutMode === 'rings') {
    assignRingLayout(graph, nodes, degrees)
  } else {
    assignCloudLayout(graph, nodes, degrees)
  }

  ensureFinitePositions(graph)
  const compactSubgraph = nodes.length <= 8
  normalizeGraphBounds(graph, {
    minExtent: compactSubgraph ? 0.96 : MIN_NORMALIZED_LAYOUT_EXTENT,
    padding: compactSubgraph ? 0.18 : NORMALIZED_LAYOUT_PADDING,
  })
}

export function createGraphModel(
  nodes: GraphNode[],
  edges: GraphEdge[],
  focusedNodeId: string | null,
  layoutMode: GraphLayoutMode,
  options?: {
    applyLayout?: boolean
  },
): MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes> {
  const graph = new MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>()
  const visualContext = createGraphVisualContext(nodes, edges, undefined, focusedNodeId)

  for (const node of nodes) {
    const visualAttributes = resolveNodeVisualAttributes(node, visualContext)
    graph.addNode(node.id, {
      x: 0,
      y: 0,
      ...visualAttributes,
    })
  }

  for (const edge of edges) {
    if (!graph.hasNode(edge.source) || !graph.hasNode(edge.target)) {
      continue
    }
    graph.addEdge(edge.source, edge.target, resolveEdgeVisualAttributes(edge, visualContext))
  }

  if (options?.applyLayout !== false) {
    applyLayout(graph, nodes, edges, layoutMode, visualContext.degreeMap)
  }
  return graph
}

export function applyGraphVisualState(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  nodes: GraphNode[],
  edges: GraphEdge[],
  focusedNodeId: string | null,
): {
  nodeIds: string[]
  edgeKeys: string[]
} {
  const degreeMap = buildDegreeMap(nodes, edges)
  const visualContext = createGraphVisualContext(nodes, edges, degreeMap, focusedNodeId)
  const nodeMap = buildNodeMap(nodes)
  const edgeMap = new Map(edges.map((edge) => [edge.id, edge]))
  const touchedNodeIds = new Set<string>()
  const touchedEdgeKeys = new Set<string>()

  graph.forEachNode((nodeId, attributes) => {
    const node = nodeMap.get(nodeId)
    if (!node) {
      return
    }

    const nextVisualAttributes = resolveNodeVisualAttributes(node, visualContext)
    const focusRoleChanged = attributes.focusRole !== nextVisualAttributes.focusRole
    const visualChanged =
      focusRoleChanged ||
      attributes.size !== nextVisualAttributes.size ||
      attributes.borderColor !== nextVisualAttributes.borderColor ||
      attributes.borderSize !== nextVisualAttributes.borderSize ||
      attributes.forceLabel !== nextVisualAttributes.forceLabel

    if (!visualChanged) {
      return
    }

    graph.replaceNodeAttributes(nodeId, {
      ...attributes,
      ...nextVisualAttributes,
      x: attributes.x,
      y: attributes.y,
    })
    touchedNodeIds.add(nodeId)
  })

  graph.forEachEdge((edgeKey, attributes) => {
    const edge = edgeMap.get(attributes.edgeId)
    if (!edge) {
      return
    }

    const nextVisualAttributes = resolveEdgeVisualAttributes(edge, visualContext)
    const focusEdgeChanged = attributes.focusEdge !== nextVisualAttributes.focusEdge
    const visualChanged =
      focusEdgeChanged ||
      attributes.size !== nextVisualAttributes.size ||
      attributes.color !== nextVisualAttributes.color

    if (!visualChanged) {
      return
    }

    graph.replaceEdgeAttributes(edgeKey, {
      ...attributes,
      ...nextVisualAttributes,
    })
    touchedEdgeKeys.add(edgeKey)
  })

  return {
    nodeIds: [...touchedNodeIds],
    edgeKeys: [...touchedEdgeKeys],
  }
}

export function resolveGraphCanvasMode(options: {
  graphStatus: GraphStatus
  nodeCount: number
  relationCount: number
  nodes: GraphNode[]
}): GraphCanvasMode {
  const { graphStatus, nodeCount, relationCount, nodes } = options
  if (graphStatus === 'failed') {
    return 'error'
  }
  if (graphStatus === 'empty' && nodeCount === 0) {
    return 'empty'
  }
  if ((graphStatus === 'building' || graphStatus === 'rebuilding') && nodeCount === 0) {
    return 'building'
  }
  if (
    relationCount === 0 &&
    nodeCount > 0 &&
    nodes.every((node) => node.nodeType === 'document') &&
    graphStatus !== 'building' &&
    graphStatus !== 'rebuilding' &&
    graphStatus !== 'empty'
  ) {
    return 'sparse'
  }
  return 'ready'
}

export function createGraphOverlayState(options: {
  nodeCount: number
  edgeCount: number
  filteredArtifactCount: number
  searchQuery?: string
  searchHits?: GraphSearchHit[]
  nodeTypeFilter?: GraphNodeType | ''
  activeLayout?: GraphLayoutMode
  showFilteredArtifacts?: boolean
  showLegend?: boolean
  showFilters?: boolean
  zoomLevel?: number
}): GraphOverlayState {
  return {
    searchQuery: options.searchQuery ?? '',
    searchHits: options.searchHits ?? [],
    nodeTypeFilter: options.nodeTypeFilter ?? '',
    activeLayout:
      options.activeLayout ?? resolveDefaultGraphLayoutMode(options.nodeCount, options.edgeCount),
    showFilteredArtifacts: options.showFilteredArtifacts ?? false,
    filteredArtifactCount: options.filteredArtifactCount,
    nodeCount: options.nodeCount,
    edgeCount: options.edgeCount,
    showLegend: options.showLegend ?? false,
    showFilters: options.showFilters ?? false,
    zoomLevel: options.zoomLevel ?? 1,
  }
}

export function createGraphInspectorState(options?: {
  focusedNodeId?: string | null
  detail?: GraphNodeDetail | null
  loading?: boolean
  error?: string | null
}): GraphInspectorState {
  return {
    focusedNodeId: options?.focusedNodeId ?? null,
    loading: options?.loading ?? false,
    error: options?.error ?? null,
    detail: options?.detail ?? null,
  }
}
