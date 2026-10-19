// Off-main-thread graph layout computation.
//
// The main thread uploads the topology once per stream (flat typed arrays +
// label/type string arrays), then requests layouts by id. Geometric layouts
// run over a typed-array `LayoutGraph` shim (no graphology build cost); the
// force layout runs the typed-array Barnes-Hut simulation in bounded slices,
// posting a transferable position snapshot after each slice so the renderer
// can stream physics into the scene while the simulation is still running.
//
// Every computed layout result is cached per (topology, layout), which makes
// switching back to an already-computed layout instant.

import { applyGraphLayout } from '@/features/graph/model/layouts'
import { LayoutGraph, type LayoutTopology } from '@/features/graph/model/layoutGraph'
import { isIterativeLayout, type GraphLayoutType } from '@/features/graph/model/config'
import { createForceSim, forceIterationBudget } from './forceSim'

export interface GraphLayoutTopologyMessage {
  type: 'topology'
  topologyId: number
  count: number
  labels: string[]
  types: string[]
  sizes: Float32Array
  neighborOffsets: Uint32Array
  neighborIndices: Uint32Array
  edgeEndpoints: Uint32Array
  edgeWeights: Float32Array
  seedPositions: Float32Array
}

interface GraphLayoutComputeMessage {
  type: 'compute'
  requestId: number
  topologyId: number
  layout: GraphLayoutType
}

interface GraphLayoutCancelMessage {
  type: 'cancel'
  requestId: number
}

export type GraphLayoutRequest =
  GraphLayoutTopologyMessage | GraphLayoutComputeMessage | GraphLayoutCancelMessage

interface GraphLayoutPositionsResponse {
  type: 'positions'
  requestId: number
  /** Interleaved `[x0, y0, ...]` in topology node order. */
  positions: Float32Array
  /** False for intermediate force-simulation snapshots. */
  final: boolean
  elapsedMs: number
}

interface GraphLayoutErrorResponse {
  type: 'error'
  requestId: number
  message: string
}

export type GraphLayoutResponse = GraphLayoutPositionsResponse | GraphLayoutErrorResponse

// The `WebWorker` lib is not part of this project's `lib` compiler option;
// declare the minimal surface this worker uses.
interface WorkerGlobalMessagePort {
  addEventListener(
    type: 'message',
    listener: (event: MessageEvent<GraphLayoutRequest>) => void,
  ): void
  postMessage(message: GraphLayoutResponse, transfer?: Transferable[]): void
}

const ctx = self as unknown as WorkerGlobalMessagePort

interface TopologyEntry {
  message: GraphLayoutTopologyMessage
  layoutTopology: LayoutTopology
  resultByLayout: Map<GraphLayoutType, Float32Array>
}

let topology: TopologyEntry | null = null
let topologyId = -1
let activeRequestId = 0

/** Target wall-clock per force slice: long enough to amortize the tree
 *  build, short enough that a layout switch lands within the time budget. */
const FORCE_SLICE_MS = 400

function postPositions(
  requestId: number,
  positions: Float32Array,
  final: boolean,
  elapsedMs: number,
): void {
  const copy = positions.slice()
  ctx.postMessage({ type: 'positions', requestId, positions: copy, final, elapsedMs }, [
    copy.buffer,
  ])
}

function computeGeometric(requestId: number, layout: GraphLayoutType, entry: TopologyEntry): void {
  const startedAt = performance.now()
  const graph = new LayoutGraph(entry.layoutTopology)
  applyGraphLayout(graph, layout)
  entry.resultByLayout.set(layout, graph.positions.slice())
  postPositions(requestId, graph.positions, true, performance.now() - startedAt)
}

async function computeForce(requestId: number, entry: TopologyEntry): Promise<void> {
  const startedAt = performance.now()
  const { message } = entry
  const sim = createForceSim({
    count: message.count,
    seedPositions: message.seedPositions,
    edgeEndpoints: message.edgeEndpoints,
    edgeWeights: message.edgeWeights,
  })
  const budget = forceIterationBudget(message.count)
  let sliceIterations = 1
  while (sim.iterationsDone < budget) {
    const iterations = Math.min(sliceIterations, budget - sim.iterationsDone)
    const sliceMs = sim.step(iterations)
    const perIteration = Math.max(0.1, sliceMs / iterations)
    sliceIterations = Math.max(1, Math.min(25, Math.round(FORCE_SLICE_MS / perIteration)))
    const final = sim.iterationsDone >= budget
    postPositions(requestId, sim.positions, final, performance.now() - startedAt)
    if (final) break
    // Yield so a superseding compute/topology message can cancel this run.
    await new Promise((resolve) => setTimeout(resolve, 0))
    if (activeRequestId !== requestId || topology !== entry) return
  }
  entry.resultByLayout.set('force', sim.positions.slice())
}

function handleTopology(message: GraphLayoutTopologyMessage): void {
  topologyId = message.topologyId
  topology = {
    message,
    layoutTopology: {
      count: message.count,
      labels: message.labels,
      types: message.types,
      sizes: message.sizes,
      neighborOffsets: message.neighborOffsets,
      neighborIndices: message.neighborIndices,
    },
    resultByLayout: new Map(),
  }
}

function handleCompute(message: GraphLayoutComputeMessage): void {
  activeRequestId = message.requestId
  if (!topology || topologyId !== message.topologyId) {
    ctx.postMessage({
      type: 'error',
      requestId: message.requestId,
      message: 'graph layout topology is not loaded',
    })
    return
  }
  const cached = topology.resultByLayout.get(message.layout)
  if (cached) {
    postPositions(message.requestId, cached, true, 0)
    return
  }
  try {
    if (isIterativeLayout(message.layout)) {
      // Async: rejections would escape the surrounding try/catch.
      computeForce(message.requestId, topology).catch((error: unknown) => {
        ctx.postMessage({
          type: 'error',
          requestId: message.requestId,
          message: error instanceof Error ? error.message : String(error),
        })
      })
    } else {
      computeGeometric(message.requestId, message.layout, topology)
    }
  } catch (error) {
    ctx.postMessage({
      type: 'error',
      requestId: message.requestId,
      message: error instanceof Error ? error.message : String(error),
    })
  }
}

ctx.addEventListener('message', (event: MessageEvent<GraphLayoutRequest>) => {
  const payload = event.data
  if (payload?.type === 'topology') handleTopology(payload)
  else if (payload?.type === 'compute') handleCompute(payload)
  else if (payload?.type === 'cancel' && payload.requestId === activeRequestId) {
    // Cancelling the active request stops the sliced force loop at its next
    // yield point (it checks activeRequestId between slices).
    activeRequestId = 0
  }
})
