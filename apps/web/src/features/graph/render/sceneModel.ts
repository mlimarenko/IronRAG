import type { GraphEdge, GraphNode } from '@/shared/types'
import { GRAPH_NODE_COLORS } from '../model/config'
import { buildNodeSizer } from '../model/graphSizing'

/**
 * Flat typed-array projection of the graph topology. Built once per stream,
 * shared by the WebGL renderer (positions / colors / sizes / edge buffer),
 * the layout worker (labels / types / CSR adjacency), and the interaction
 * layer (spatial hit-testing, neighbor highlighting). Node identity is an
 * array index; `ids` / `indexById` translate to the UUID world of the
 * inspector and the URL.
 */
export interface GraphSceneModel {
  count: number
  ids: string[]
  indexById: Map<string, number>
  labels: string[]
  types: string[]
  sizes: Float32Array
  /** RGBA per node, 0-255. */
  colors: Uint8Array
  degrees: Uint32Array
  /** Deterministic first-frame positions (BFS-ordered phyllotaxis). */
  seedPositions: Float32Array
  /** CSR adjacency (undirected, deduplicated, self-loops removed). */
  neighborOffsets: Uint32Array
  neighborIndices: Uint32Array
  /** Deduplicated undirected edges, sorted by weight descending. */
  edgeCount: number
  edgeEndpoints: Uint32Array
  edgeWeights: Float32Array
  /** Node index with the highest degree (bench + label anchor). */
  maxDegreeIndex: number
  /** Stable signature of the topology for position caches. */
  topologyKey: string
}

function parseHexColor(hex: string): [number, number, number] {
  const value = Number.parseInt(hex.slice(1), 16)
  return [(value >> 16) & 0xff, (value >> 8) & 0xff, value & 0xff]
}

function hashString(seedHash: number, text: string): number {
  let hash = seedHash
  for (let index = 0; index < text.length; index += 1) {
    hash ^= text.codePointAt(index) ?? 0
    hash = Math.imul(hash, 16777619)
  }
  return hash >>> 0
}

function buildTopologyKey(ids: string[], endpoints: Uint32Array): string {
  let hash = 2166136261
  const stride = Math.max(1, Math.floor(ids.length / 64))
  for (let index = 0; index < ids.length; index += stride) {
    hash = hashString(hash, ids[index] ?? '')
  }
  // Sample the edge set too: two topologies over the same nodes with the
  // same edge COUNT but different edges must not share cached positions.
  const edgeStride = Math.max(1, Math.floor(endpoints.length / 256))
  for (let index = 0; index < endpoints.length; index += edgeStride) {
    hash ^= endpoints[index]! + 0x9e3779b9
    hash = Math.imul(hash, 16777619) >>> 0
  }
  return `${ids.length}:${endpoints.length / 2}:${hash.toString(16)}`
}

/**
 * Deduplicate edges into undirected index pairs, merging weights. The wire
 * topology can carry the same relation in both directions and repeated
 * document links; the render/layout pipeline needs each pair once.
 */
function dedupeEdges(
  edges: GraphEdge[],
  indexById: Map<string, number>,
  count: number,
): { endpoints: Uint32Array; weights: Float32Array } {
  const weightByPair = new Map<number, number>()
  for (const edge of edges) {
    const source = indexById.get(edge.sourceId)
    const target = indexById.get(edge.targetId)
    if (source === undefined || target === undefined || source === target) continue
    const low = Math.min(source, target)
    const high = Math.max(source, target)
    const key = low * count + high
    weightByPair.set(key, (weightByPair.get(key) ?? 0) + (edge.weight || 1))
  }

  const pairCount = weightByPair.size
  const endpoints = new Uint32Array(pairCount * 2)
  const weights = new Float32Array(pairCount)
  let cursor = 0
  for (const [key, weight] of weightByPair) {
    endpoints[cursor * 2] = Math.floor(key / count)
    endpoints[cursor * 2 + 1] = key % count
    weights[cursor] = weight
    cursor += 1
  }

  // Sort by weight descending so an edge-density cap keeps the strongest
  // links: rendering the first K edges of the buffer is then a meaningful
  // "most important K" rather than an arbitrary sample.
  const order = Array.from({ length: pairCount }, (_, index) => index).sort(
    (left, right) => weights[right]! - weights[left]!,
  )
  const sortedEndpoints = new Uint32Array(pairCount * 2)
  const sortedWeights = new Float32Array(pairCount)
  for (let position = 0; position < pairCount; position += 1) {
    const from = order[position]!
    sortedEndpoints[position * 2] = endpoints[from * 2]!
    sortedEndpoints[position * 2 + 1] = endpoints[from * 2 + 1]!
    sortedWeights[position] = weights[from]!
  }
  return { endpoints: sortedEndpoints, weights: sortedWeights }
}

function buildCsr(
  count: number,
  endpoints: Uint32Array,
): { offsets: Uint32Array; neighbors: Uint32Array; degrees: Uint32Array } {
  const degrees = new Uint32Array(count)
  const edgeCount = endpoints.length / 2
  for (let edge = 0; edge < edgeCount; edge += 1) {
    degrees[endpoints[edge * 2]!]! += 1
    degrees[endpoints[edge * 2 + 1]!]! += 1
  }
  const offsets = new Uint32Array(count + 1)
  for (let node = 0; node < count; node += 1) {
    offsets[node + 1] = offsets[node]! + degrees[node]!
  }
  const neighbors = new Uint32Array(offsets[count]!)
  const cursor = offsets.slice(0, count)
  for (let edge = 0; edge < edgeCount; edge += 1) {
    const source = endpoints[edge * 2]!
    const target = endpoints[edge * 2 + 1]!
    neighbors[cursor[source]!] = target
    cursor[source]! += 1
    neighbors[cursor[target]!] = source
    cursor[target]! += 1
  }
  return { offsets, neighbors, degrees }
}

/**
 * Seed layout: phyllotaxis spiral in BFS order from the highest-degree
 * roots. BFS ordering keeps each connected neighborhood contiguous on the
 * spiral, so the very first painted frame already shows cluster structure
 * and the force simulation starts near its basin instead of from noise.
 */
function buildSeedPositions(
  count: number,
  degrees: Uint32Array,
  offsets: Uint32Array,
  neighbors: Uint32Array,
): Float32Array {
  const positions = new Float32Array(count * 2)
  const visited = new Uint8Array(count)
  const queue = new Uint32Array(count)
  const roots = Array.from({ length: count }, (_, index) => index).sort(
    (left, right) => degrees[right]! - degrees[left]!,
  )

  let placed = 0
  const place = (node: number): void => {
    const angle = placed * 2.399963229728653
    const radius = Math.sqrt(placed + 1) * 6
    positions[node * 2] = Math.cos(angle) * radius
    positions[node * 2 + 1] = Math.sin(angle) * radius
    placed += 1
  }

  for (const root of roots) {
    if (visited[root]) continue
    visited[root] = 1
    let head = 0
    let tail = 0
    queue[tail] = root
    tail += 1
    while (head < tail) {
      const node = queue[head]!
      head += 1
      place(node)
      const end = offsets[node + 1]!
      for (let cursor = offsets[node]!; cursor < end; cursor += 1) {
        const neighbor = neighbors[cursor]!
        if (visited[neighbor]) continue
        visited[neighbor] = 1
        queue[tail] = neighbor
        tail += 1
      }
    }
  }
  return positions
}

export function buildSceneModel(nodes: GraphNode[], edges: GraphEdge[]): GraphSceneModel {
  const count = nodes.length
  const ids: string[] = new Array(count)
  const labels: string[] = new Array(count)
  const types: string[] = new Array(count)
  const indexById = new Map<string, number>()
  for (let index = 0; index < count; index += 1) {
    const node = nodes[index]!
    ids[index] = node.id
    labels[index] = node.label
    types[index] = node.type
    indexById.set(node.id, index)
  }

  const { endpoints, weights } = dedupeEdges(edges, indexById, Math.max(1, count))
  const { offsets, neighbors, degrees } = buildCsr(count, endpoints)

  const sizeNode = buildNodeSizer(nodes)
  const sizes = new Float32Array(count)
  const colors = new Uint8Array(count * 4)
  const fallbackColor = parseHexColor(GRAPH_NODE_COLORS.entity!)
  for (let index = 0; index < count; index += 1) {
    const node = nodes[index]!
    sizes[index] = sizeNode(node)
    const hex = GRAPH_NODE_COLORS[node.type]
    const [red, green, blue] = hex ? parseHexColor(hex) : fallbackColor
    colors[index * 4] = red
    colors[index * 4 + 1] = green
    colors[index * 4 + 2] = blue
    colors[index * 4 + 3] = 255
  }

  let maxDegreeIndex = 0
  for (let index = 1; index < count; index += 1) {
    if (degrees[index]! > degrees[maxDegreeIndex]!) maxDegreeIndex = index
  }

  return {
    count,
    ids,
    indexById,
    labels,
    types,
    sizes,
    colors,
    degrees,
    seedPositions: buildSeedPositions(count, degrees, offsets, neighbors),
    neighborOffsets: offsets,
    neighborIndices: neighbors,
    edgeCount: weights.length,
    edgeEndpoints: endpoints,
    edgeWeights: weights,
    maxDegreeIndex,
    topologyKey: buildTopologyKey(ids, endpoints),
  }
}
