import type Graph from 'graphology'
import forceAtlas2 from 'graphology-layout-forceatlas2'

/**
 * ForceAtlas2 iteration budget by graph size. More iterations settle a cleaner
 * layout but cost linearly; large graphs also enable Barnes-Hut via
 * `inferSettings`, so fewer passes still converge acceptably. Tunable against
 * real-scale profiling.
 */
export function forceLayoutIterations(order: number): number {
  if (order > 20000) return 60
  if (order > 5000) return 120
  if (order > 1000) return 250
  return 400
}

/**
 * Run weighted ForceAtlas2 in place. Kept in a worker-only module so
 * `graphology-layout-forceatlas2` never enters the main app bundle.
 *
 * Preconditions the caller MUST satisfy:
 *   - the graph already carries a non-degenerate SEED layout — FA2 started from
 *     all-(0,0) positions diverges to NaN on the first pass;
 *   - edges carry a numeric `weight` attribute, so stronger links pull their
 *     endpoints closer (the LightRAG-style "distance ~ connection strength").
 */
export function runForceSimulation(graph: Graph): void {
  if (graph.order === 0) return
  forceAtlas2.assign(graph, {
    iterations: forceLayoutIterations(graph.order),
    getEdgeWeight: 'weight',
    settings: {
      ...forceAtlas2.inferSettings(graph),
      edgeWeightInfluence: 1,
    },
  })
}
