// Promise-based client for the graph layout worker.
//
// A single lazy-instantiated worker is reused across calls. The scene model
// topology is uploaded once (tracked via WeakMap) and layouts are then
// requested by id. Responses stream: the force layout posts several
// intermediate position snapshots before the final one, and stale requests
// (superseded by a newer layout choice) are dropped on arrival.

import type {
  GraphLayoutRequest,
  GraphLayoutResponse,
  GraphLayoutTopologyMessage,
} from './graphLayout.worker'
import type { GraphLayoutType } from '@/features/graph/model/config'
import type { GraphSceneModel } from '@/features/graph/render/sceneModel'

// `?worker` is a Vite-specific suffix that produces a module worker
// constructor bound to the referenced file, bundled as its own chunk.
import GraphLayoutWorkerCtor from './graphLayout.worker?worker'

export interface LayoutStream {
  /** Resolves after the final position snapshot (or rejects on error). */
  done: Promise<void>
  /** Stop delivering snapshots for this request. */
  cancel(): void
}

interface PendingRequest {
  onPositions: (positions: Float32Array, final: boolean) => void
  resolve: () => void
  reject: (reason: unknown) => void
}

let sharedWorker: Worker | null = null
let nextRequestId = 1
let nextTopologyId = 1
let uploadedTopologyIds = new WeakMap<GraphSceneModel, number>()
const pending = new Map<number, PendingRequest>()

function ensureWorker(): Worker {
  if (sharedWorker) return sharedWorker
  const worker = new GraphLayoutWorkerCtor()
  worker.addEventListener('message', (event: MessageEvent<GraphLayoutResponse>) => {
    const data = event.data
    const entry = pending.get(data.requestId)
    if (!entry) return
    if (data.type === 'error') {
      pending.delete(data.requestId)
      entry.reject(new Error(data.message))
      return
    }
    entry.onPositions(data.positions, data.final)
    if (data.final) {
      pending.delete(data.requestId)
      entry.resolve()
    }
  })
  worker.addEventListener('error', (event) => {
    // Blow away every outstanding request — the worker is in an
    // indeterminate state after an unhandled error.
    const error = new Error(`graph layout worker error: ${event.message}`)
    for (const entry of pending.values()) {
      entry.reject(error)
    }
    pending.clear()
    sharedWorker = null
    uploadedTopologyIds = new WeakMap()
  })
  sharedWorker = worker
  return worker
}

function ensureTopologyUploaded(worker: Worker, model: GraphSceneModel): number {
  const uploaded = uploadedTopologyIds.get(model)
  if (uploaded != null) return uploaded
  const topologyId = nextTopologyId
  nextTopologyId += 1
  uploadedTopologyIds.set(model, topologyId)
  const message: GraphLayoutTopologyMessage = {
    type: 'topology',
    topologyId,
    count: model.count,
    labels: model.labels,
    types: model.types,
    sizes: model.sizes,
    neighborOffsets: model.neighborOffsets,
    neighborIndices: model.neighborIndices,
    edgeEndpoints: model.edgeEndpoints,
    edgeWeights: model.edgeWeights,
    seedPositions: model.seedPositions,
  }
  // Arrays are structured-cloned (NOT transferred): the renderer keeps using
  // the same buffers on the main thread.
  worker.postMessage(message)
  return topologyId
}

/**
 * Request a layout for the scene model. `onPositions` fires once for
 * geometric layouts and multiple times for the force simulation (snapshots
 * with `final: false`, then the settled result with `final: true`).
 */
export function streamGraphLayout(params: {
  model: GraphSceneModel
  layout: GraphLayoutType
  onPositions: (positions: Float32Array, final: boolean) => void
}): LayoutStream {
  const worker = ensureWorker()
  const topologyId = ensureTopologyUploaded(worker, params.model)
  const requestId = nextRequestId
  nextRequestId += 1

  let cancelled = false
  const done = new Promise<void>((resolve, reject) => {
    pending.set(requestId, {
      onPositions: (positions, final) => {
        if (!cancelled) params.onPositions(positions, final)
      },
      resolve,
      reject,
    })
  })
  const message: GraphLayoutRequest = {
    type: 'compute',
    requestId,
    topologyId,
    layout: params.layout,
  }
  worker.postMessage(message)

  return {
    done,
    cancel() {
      cancelled = true
      // Tell the worker too: without this a superseded force run keeps
      // simulating to its full iteration budget in the background.
      worker.postMessage({ type: 'cancel', requestId } satisfies GraphLayoutRequest)
      const entry = pending.get(requestId)
      if (entry) {
        pending.delete(requestId)
        entry.resolve()
      }
    },
  }
}
