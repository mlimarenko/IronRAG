// Promise-based client for the graph layout worker.
//
// A single lazy-instantiated worker is reused across calls; the worker
// is cheap to keep alive and re-spawning it on every layout request
// would defeat the point of offloading. Requests are multiplexed via a
// monotonically-increasing `requestId` so in-flight stale layout
// requests (e.g. the user toggled layouts twice in a row) are
// discarded when a newer one lands.

import type {
  GraphLayoutRequest,
  GraphLayoutRequestNode,
  GraphLayoutResponse,
  GraphLayoutErrorResponse,
} from './graphLayout.worker';
import type { GraphLayoutType } from '@/components/graph/config';

// `?worker` is a Vite-specific suffix that produces a module worker
// constructor bound to the referenced file. The underlying worker is
// built as a standalone chunk, so `graphology` and the layouts module
// are bundled into the worker script instead of into the main app
// bundle.
import GraphLayoutWorkerCtor from './graphLayout.worker?worker';

type PendingResolver = {
  resolve: (value: GraphLayoutResponse) => void;
  reject: (reason: unknown) => void;
};

let sharedWorker: Worker | null = null;
let nextRequestId = 1;
const pending = new Map<number, PendingResolver>();

function ensureWorker(): Worker {
  if (sharedWorker) return sharedWorker;
  const worker = new GraphLayoutWorkerCtor();
  worker.addEventListener(
    'message',
    (event: MessageEvent<GraphLayoutResponse | GraphLayoutErrorResponse>) => {
      const data = event.data;
      const entry = pending.get(data.requestId);
      if (!entry) return;
      pending.delete(data.requestId);
      if (data.type === 'error') {
        entry.reject(new Error(data.message));
      } else {
        entry.resolve(data);
      }
    },
  );
  worker.addEventListener('error', (event) => {
    // Blow away every outstanding request — the worker is in an
    // indeterminate state after an unhandled error.
    const error = new Error(`graph layout worker error: ${event.message}`);
    for (const entry of pending.values()) {
      entry.reject(error);
    }
    pending.clear();
    sharedWorker = null;
  });
  sharedWorker = worker;
  return worker;
}

export function computeGraphLayoutOffThread(params: {
  nodes: GraphLayoutRequestNode[];
  edges: Array<{ sourceId: string; targetId: string }>;
  layout: GraphLayoutType;
}): Promise<GraphLayoutResponse> {
  const worker = ensureWorker();
  const requestId = nextRequestId;
  nextRequestId += 1;
  const message: GraphLayoutRequest = {
    type: 'compute',
    requestId,
    layout: params.layout,
    nodes: params.nodes,
    edges: params.edges,
  };
  return new Promise<GraphLayoutResponse>((resolve, reject) => {
    pending.set(requestId, { resolve, reject });
    worker.postMessage(message);
  });
}

/// Free the shared worker when the caller knows no more layout
/// computation will happen (e.g. the graph page unmounts on route
/// change). Not strictly required — modern browsers reap idle workers
/// — but it releases the Graphology import chunk from memory
/// immediately.
export function terminateGraphLayoutWorker(): void {
  if (!sharedWorker) return;
  sharedWorker.terminate();
  sharedWorker = null;
  pending.clear();
}
