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

import Graph from 'graphology';
import { applyGraphLayout } from '@/features/graph/model/layouts';
import type { GraphLayoutType } from '@/features/graph/model/config';

export interface GraphLayoutRequestNode {
  id: string;
  nodeType: string;
  size: number;
  label: string;
}

export interface GraphLayoutRequest {
  type: 'compute';
  requestId: number;
  topologyId?: number | undefined;
  layout: GraphLayoutType;
  nodes?: GraphLayoutRequestNode[] | undefined;
  edges?: Array<{ sourceId: string; targetId: string }> | undefined;
}

export interface GraphLayoutResponse {
  type: 'result';
  requestId: number;
  /** Interleaved `[x0, y0, x1, y1, ...]` matching request node order. */
  positions: Float32Array;
  elapsedMs: number;
}

export interface GraphLayoutErrorResponse {
  type: 'error';
  requestId: number;
  message: string;
}

// The `WebWorker` lib (which declares `DedicatedWorkerGlobalScope`) is not
// part of this project's `lib` compiler option (only `ES2020`/`DOM` are, so
// the same tsconfig also type-checks DOM-side code). Declare the minimal
// surface this worker actually uses instead of pulling in the global type.
interface WorkerGlobalMessagePort {
  addEventListener(
    type: 'message',
    listener: (event: MessageEvent<GraphLayoutRequest>) => void,
  ): void;
  postMessage(
    message: GraphLayoutResponse | GraphLayoutErrorResponse,
    transfer?: Transferable[],
  ): void;
}

const ctx = self as unknown as WorkerGlobalMessagePort;
let cachedTopologyId: number | null = null;
let cachedNodes: GraphLayoutRequestNode[] | null = null;
let cachedEdges: Array<{ sourceId: string; targetId: string }> | null = null;

ctx.addEventListener('message', (event: MessageEvent<GraphLayoutRequest>) => {
  const payload = event.data;
  if (!payload || payload.type !== 'compute') return;
  try {
    if (payload.nodes && payload.edges) {
      cachedTopologyId = payload.topologyId ?? null;
      cachedNodes = payload.nodes;
      cachedEdges = payload.edges;
    }
    if (
      !cachedNodes ||
      !cachedEdges ||
      (payload.topologyId != null && cachedTopologyId !== payload.topologyId)
    ) {
      throw new Error('graph layout topology is not loaded');
    }

    const started = performance.now();
    const graph = new Graph();

    for (const node of cachedNodes) {
      graph.addNode(node.id, {
        x: 0,
        y: 0,
        // `sortNodesByImportance` used by several layouts reads `size`
        // and `label` as tiebreakers; pass the real values so the
        // ordering matches the main-thread build exactly.
        size: node.size,
        nodeType: node.nodeType,
        label: node.label,
      });
    }

    const seen = new Set<string>();
    for (const edge of cachedEdges) {
      if (edge.sourceId === edge.targetId) continue;
      if (!graph.hasNode(edge.sourceId) || !graph.hasNode(edge.targetId)) continue;
      const key = `${edge.sourceId}->${edge.targetId}`;
      if (seen.has(key)) continue;
      seen.add(key);
      try {
        graph.addEdge(edge.sourceId, edge.targetId);
      } catch {
        // Parallel edge — skip silently. Same semantics as the main
        // thread path in SigmaGraph.tsx.
      }
    }

    applyGraphLayout(graph, payload.layout);

    const order = cachedNodes.length;
    const positions = new Float32Array(order * 2);
    for (let i = 0; i < cachedNodes.length; i += 1) {
      const node = cachedNodes[i];
      if (!node) continue;
      const attrs = graph.getNodeAttributes(node.id);
      positions[i * 2] = (attrs.x as number | undefined) ?? 0;
      positions[i * 2 + 1] = (attrs.y as number | undefined) ?? 0;
    }

    const response: GraphLayoutResponse = {
      type: 'result',
      requestId: payload.requestId,
      positions,
      elapsedMs: performance.now() - started,
    };
    ctx.postMessage(response, [positions.buffer]);
  } catch (error) {
    const response: GraphLayoutErrorResponse = {
      type: 'error',
      requestId: payload.requestId,
      message: error instanceof Error ? error.message : String(error),
    };
    ctx.postMessage(response);
  }
});
