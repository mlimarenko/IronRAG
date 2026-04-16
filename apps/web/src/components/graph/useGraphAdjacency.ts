import { useMemo } from 'react';
import type { GraphEdge, GraphNode } from '@/types';

export type GraphNeighborhood = {
  /** All neighbors of `nodeId`, resolved into full node objects, deduped. */
  nodes: GraphNode[];
  /** Raw neighbor IDs in edge-insertion order. Callers use `.length` for connection counts. */
  ids: string[];
};

export type GraphAdjacencyIndex = {
  /** Maps `nodeId` → array of neighbor IDs (deduped, self-loops removed). */
  neighborIds: Map<string, string[]>;
  /** Maps `nodeId` → `GraphNode` for O(1) lookup when resolving neighbors. */
  nodeById: Map<string, GraphNode>;
  /** Resolve the neighborhood of a node in O(k) where k = neighbor count. */
  neighborhoodOf(nodeId: string | null, limit?: number): GraphNeighborhood;
};

/**
 * Build a stable adjacency index once per (nodes, edges) pair. The GraphPage
 * inspector needs to know which nodes connect to the selected node; computing
 * that inline inside render via `allEdges.filter(...).map(...)` is O(E + E*N)
 * on every render (re-running on unrelated state changes like summary
 * expand/collapse or search typing). Memoizing the adjacency map turns
 * inspector lookup into O(k) per selection where k is the actual neighbor
 * count — usually <20 regardless of graph size.
 */
export function useGraphAdjacency(
  nodes: GraphNode[],
  edges: GraphEdge[],
): GraphAdjacencyIndex {
  return useMemo(() => {
    const nodeById = new Map<string, GraphNode>();
    for (const node of nodes) {
      nodeById.set(node.id, node);
    }

    const neighborIds = new Map<string, string[]>();
    const seen = new Map<string, Set<string>>();
    for (const edge of edges) {
      const { sourceId, targetId } = edge;
      if (!sourceId || !targetId || sourceId === targetId) continue;

      let sourceNeighbors = neighborIds.get(sourceId);
      if (!sourceNeighbors) {
        sourceNeighbors = [];
        neighborIds.set(sourceId, sourceNeighbors);
        seen.set(sourceId, new Set());
      }
      const sourceSeen = seen.get(sourceId)!;
      if (!sourceSeen.has(targetId)) {
        sourceNeighbors.push(targetId);
        sourceSeen.add(targetId);
      }

      let targetNeighbors = neighborIds.get(targetId);
      if (!targetNeighbors) {
        targetNeighbors = [];
        neighborIds.set(targetId, targetNeighbors);
        seen.set(targetId, new Set());
      }
      const targetSeen = seen.get(targetId)!;
      if (!targetSeen.has(sourceId)) {
        targetNeighbors.push(sourceId);
        targetSeen.add(sourceId);
      }
    }

    const neighborhoodOf = (nodeId: string | null, limit?: number): GraphNeighborhood => {
      if (!nodeId) return { nodes: [], ids: [] };
      const ids = neighborIds.get(nodeId) ?? [];
      const sliced = typeof limit === 'number' ? ids.slice(0, limit) : ids;
      const resolved: GraphNode[] = [];
      for (const id of sliced) {
        const node = nodeById.get(id);
        if (node) resolved.push(node);
      }
      return { nodes: resolved, ids };
    };

    return { neighborIds, nodeById, neighborhoodOf };
  }, [nodes, edges]);
}
