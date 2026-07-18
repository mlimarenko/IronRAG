// Off-main-thread graph layout computation.
//
// At 25 k nodes + 80 k edges the Graphology build + layout pass runs
// 700-1700 ms on a typical laptop. Running it on the main thread
// triggers Chrome's "page is slowing down" warning and freezes the UI
// shell while the user is waiting for the graph to appear. This worker
// offloads everything that does not touch the DOM:
//
//   1. Build a scratch Graphology instance from the slim node/edge
//      payload the main thread sends.
//   2. Compute the requested layout via `applyGraphLayout`.
//   3. Extract the resulting (x, y) pairs into a `Float32Array` and
//      post them back as a transferable buffer (zero-copy).
//
// The main thread then constructs its own Graphology instance using
// those precomputed positions and hands it to Sigma. The worker's
// Graphology instance is thrown away — the double-build is the price
// we pay for moving the expensive layout step off the critical frame
// path, and on dense graphs the wall-clock wins dwarf that cost.

import Graph from 'graphology'
import { applyGraphLayout } from '@/features/graph/model/layouts'
import { runForceSimulation } from './forceSimulation'
import { isIterativeLayout, type GraphLayoutType } from '@/features/graph/model/config'

export interface GraphLayoutRequestNode {
  id: string
  nodeType: string
  size: number
  label: string
}

export interface GraphLayoutRequestEdge {
  sourceId: string
  targetId: string
  /** Relation / document support count; drives weighted ForceAtlas2 (stronger
   *  links pull nodes closer). Geometric layouts ignore it. */
  weight?: number | undefined
}

export interface GraphLayoutRequest {
  type: 'compute'
  requestId: number
  topologyId?: number | undefined
  layout: GraphLayoutType
  nodes?: GraphLayoutRequestNode[] | undefined
  edges?: GraphLayoutRequestEdge[] | undefined
}

export interface GraphLayoutResponse {
  type: 'result'
  requestId: number
  /** Interleaved `[x0, y0, x1, y1, ...]` matching request node order. */
  positions: Float32Array
  elapsedMs: number
}

export interface GraphLayoutErrorResponse {
  type: 'error'
  requestId: number
  message: string
}

// The `WebWorker` lib (which declares `DedicatedWorkerGlobalScope`) is not
// part of this project's `lib` compiler option (only `ES2020`/`DOM` are, so
// the same tsconfig also type-checks DOM-side code). Declare the minimal
// surface this worker actually uses instead of pulling in the global type.
interface WorkerGlobalMessagePort {
  addEventListener(
    type: 'message',
    listener: (event: MessageEvent<GraphLayoutRequest>) => void,
  ): void
  postMessage(
    message: GraphLayoutResponse | GraphLayoutErrorResponse,
    transfer?: Transferable[],
  ): void
}

const ctx = self as unknown as WorkerGlobalMessagePort
let cachedTopologyId: number | null = null
let cachedNodes: GraphLayoutRequestNode[] | null = null
let cachedEdges: GraphLayoutRequestEdge[] | null = null

function updateTopologyCache(payload: GraphLayoutRequest): void {
  if (!payload.nodes || !payload.edges) {
    return
  }
  cachedTopologyId = payload.topologyId ?? null
  cachedNodes = payload.nodes
  cachedEdges = payload.edges
}

function loadCachedTopology(payload: GraphLayoutRequest): {
  nodes: GraphLayoutRequestNode[]
  edges: GraphLayoutRequestEdge[]
} {
  updateTopologyCache(payload)
  if (
    !cachedNodes ||
    !cachedEdges ||
    (payload.topologyId != null && cachedTopologyId !== payload.topologyId)
  ) {
    throw new Error('graph layout topology is not loaded')
  }
  return { nodes: cachedNodes, edges: cachedEdges }
}

function buildLayoutGraph(nodes: GraphLayoutRequestNode[], edges: GraphLayoutRequestEdge[]): Graph {
  const graph = new Graph()
  for (const node of nodes) {
    graph.addNode(node.id, {
      x: 0,
      y: 0,
      size: node.size,
      nodeType: node.nodeType,
      label: node.label,
    })
  }
  for (const edge of edges) {
    addLayoutEdge(graph, edge)
  }
  return graph
}

function addLayoutEdge(graph: Graph, edge: GraphLayoutRequestEdge): void {
  if (
    edge.sourceId === edge.targetId ||
    !graph.hasNode(edge.sourceId) ||
    !graph.hasNode(edge.targetId)
  ) {
    return
  }
  const weight = edge.weight ?? 1
  if (graph.hasEdge(edge.sourceId, edge.targetId)) {
    graph.updateEdgeAttribute(
      edge.sourceId,
      edge.targetId,
      'weight',
      (current) => (typeof current === 'number' ? current : 0) + weight,
    )
    return
  }
  try {
    graph.addEdge(edge.sourceId, edge.targetId, { weight })
  } catch {
    // Graphology can reject a parallel edge inserted by another route.
  }
}

function serializePositions(graph: Graph, nodes: GraphLayoutRequestNode[]): Float32Array {
  const positions = new Float32Array(nodes.length * 2)
  nodes.forEach((node, index) => {
    const attrs = graph.getNodeAttributes(node.id)
    positions[index * 2] = (attrs.x as number | undefined) ?? 0
    positions[index * 2 + 1] = (attrs.y as number | undefined) ?? 0
  })
  return positions
}

ctx.addEventListener('message', (event: MessageEvent<GraphLayoutRequest>) => {
  const payload = event.data
  if (payload?.type !== 'compute') return
  try {
    const topology = loadCachedTopology(payload)
    const started = performance.now()
    const graph = buildLayoutGraph(topology.nodes, topology.edges)

    applyGraphLayout(graph, payload.layout)
    if (isIterativeLayout(payload.layout)) {
      runForceSimulation(graph)
    }

    const positions = serializePositions(graph, topology.nodes)
    const response: GraphLayoutResponse = {
      type: 'result',
      requestId: payload.requestId,
      positions,
      elapsedMs: performance.now() - started,
    }
    ctx.postMessage(response, [positions.buffer])
  } catch (error) {
    const response: GraphLayoutErrorResponse = {
      type: 'error',
      requestId: payload.requestId,
      message: error instanceof Error ? error.message : String(error),
    }
    ctx.postMessage(response)
  }
})
