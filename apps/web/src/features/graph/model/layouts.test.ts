import Graph from 'graphology';
import { describe, expect, it } from 'vitest';

import { GRAPH_LAYOUT_OPTIONS } from './config';
import { applyGraphLayout } from './layouts';

function buildSyntheticGraph(): Graph {
  const graph = new Graph();
  const nodes = [
    ['doc-1', 'document', 7],
    ['doc-2', 'document', 4],
    ['entity-1', 'concept', 8],
    ['entity-2', 'person', 5],
    ['entity-3', 'organization', 6],
    ['entity-4', 'artifact', 3],
    ['entity-5', 'location', 2],
    ['entity-6', 'process', 1],
  ] as const;

  nodes.forEach(([id, nodeType, size]) => {
    graph.addNode(id, {
      label: id,
      nodeType,
      size,
      x: 0,
      y: 0,
    });
  });

  graph.addEdge('doc-1', 'entity-1');
  graph.addEdge('doc-1', 'entity-2');
  graph.addEdge('doc-2', 'entity-3');
  graph.addEdge('entity-1', 'entity-2');
  graph.addEdge('entity-1', 'entity-3');
  graph.addEdge('entity-3', 'entity-4');
  graph.addEdge('entity-4', 'entity-5');

  return graph;
}

function collectPositions(graph: Graph): Record<string, [number, number]> {
  const positions: Record<string, [number, number]> = {};
  graph.forEachNode((node, attrs) => {
    positions[node] = [attrs.x as number, attrs.y as number];
  });
  return positions;
}

describe('graph layouts', () => {
  it.each(GRAPH_LAYOUT_OPTIONS)('$id assigns finite deterministic positions', (option) => {
    const first = buildSyntheticGraph();
    const second = buildSyntheticGraph();

    applyGraphLayout(first, option.id);
    applyGraphLayout(second, option.id);

    const firstPositions = collectPositions(first);
    const secondPositions = collectPositions(second);
    expect(firstPositions).toEqual(secondPositions);

    for (const [x, y] of Object.values(firstPositions)) {
      expect(Number.isFinite(x)).toBe(true);
      expect(Number.isFinite(y)).toBe(true);
    }

    const uniquePositions = new Set(
      Object.values(firstPositions).map(([x, y]) => `${x.toFixed(6)}:${y.toFixed(6)}`),
    );
    expect(uniquePositions.size).toBeGreaterThan(1);
  });
});
