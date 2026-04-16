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
import { applyGraphLayout } from '@/components/graph/layouts';
import type { GraphLayoutType } from '@/components/graph/config';

export interface GraphLayoutRequestNode {
  id: string;
  nodeType: string;
  size: number;
  label: string;
}

export interface GraphLayoutRequest {
  type: 'compute';
  requestId: number;
  layout: GraphLayoutType;
  nodes: GraphLayoutRequestNode[];
  edges: Array<{ sourceId: string; targetId: string }>;
}

export interface GraphLayoutResponse {
  type: 'result';
  requestId: number;
  ids: string[];
  /** Interleaved `[x0, y0, x1, y1, ...]` matching `ids` element-wise. */
  positions: Float32Array;
  elapsedMs: number;
}

export interface GraphLayoutErrorResponse {
  type: 'error';
  requestId: number;
  message: string;
}

type WorkerResponse = GraphLayoutResponse | GraphLayoutErrorResponse;

const ctx = self as unknown as DedicatedWorkerGlobalScope;

ctx.addEventListener('message', (event: MessageEvent<GraphLayoutRequest>) => {
  const payload = event.data;
  if (!payload || payload.type !== 'compute') return;
  try {
    const started = performance.now();
    const graph = new Graph();

    for (const node of payload.nodes) {
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
    for (const edge of payload.edges) {
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

    const order = graph.order;
    const ids = new Array<string>(order);
    const positions = new Float32Array(order * 2);
    let i = 0;
    graph.forEachNode((nodeId, attrs) => {
      ids[i] = nodeId;
      positions[i * 2] = (attrs.x as number | undefined) ?? 0;
      positions[i * 2 + 1] = (attrs.y as number | undefined) ?? 0;
      i += 1;
    });

    const response: GraphLayoutResponse = {
      type: 'result',
      requestId: payload.requestId,
      ids,
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

// Silence the "WorkerResponse is never used" lint while keeping the
// union type exported for the client's type imports.
export type GraphLayoutWorkerResponse = WorkerResponse;
