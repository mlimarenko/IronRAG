import { describe, expect, it } from 'vitest'

import { GRAPH_LAYOUT_OPTIONS } from './config'
import { applyGraphLayout } from './layouts'
import { LayoutGraph, type LayoutTopology } from './layoutGraph'

/**
 * Synthetic 8-node topology over the typed-array LayoutGraph shim — the same
 * structure every geometric layout receives from the worker.
 */
function buildSyntheticTopology(): LayoutTopology {
  const nodes = [
    ['doc-1', 'document', 7],
    ['doc-2', 'document', 4],
    ['entity-1', 'concept', 8],
    ['entity-2', 'person', 5],
    ['entity-3', 'organization', 6],
    ['entity-4', 'artifact', 3],
    ['entity-5', 'location', 2],
    ['entity-6', 'process', 1],
  ] as const
  const edges: Array<[number, number]> = [
    [0, 2],
    [0, 3],
    [1, 4],
    [2, 3],
    [2, 4],
    [4, 5],
    [5, 6],
  ]

  const count = nodes.length
  const degrees = new Uint32Array(count)
  for (const [source, target] of edges) {
    degrees[source]! += 1
    degrees[target]! += 1
  }
  const neighborOffsets = new Uint32Array(count + 1)
  for (let node = 0; node < count; node += 1) {
    neighborOffsets[node + 1] = neighborOffsets[node]! + degrees[node]!
  }
  const neighborIndices = new Uint32Array(neighborOffsets[count]!)
  const cursor = neighborOffsets.slice(0, count)
  for (const [source, target] of edges) {
    neighborIndices[cursor[source]!] = target
    cursor[source]! += 1
    neighborIndices[cursor[target]!] = source
    cursor[target]! += 1
  }

  return {
    count,
    labels: nodes.map(([id]) => id),
    types: nodes.map(([, type]) => type),
    sizes: new Float32Array(nodes.map(([, , size]) => size)),
    neighborOffsets,
    neighborIndices,
  }
}

function collectPositions(graph: LayoutGraph): Record<string, [number, number]> {
  const positions: Record<string, [number, number]> = {}
  graph.forEachNode((node) => {
    positions[node] = [
      graph.getNodeAttribute(node, 'x') as number,
      graph.getNodeAttribute(node, 'y') as number,
    ]
  })
  return positions
}

describe('graph layouts', () => {
  it.each(GRAPH_LAYOUT_OPTIONS)('$id assigns finite deterministic positions', (option) => {
    const first = new LayoutGraph(buildSyntheticTopology())
    const second = new LayoutGraph(buildSyntheticTopology())

    applyGraphLayout(first, option.id)
    applyGraphLayout(second, option.id)

    const firstPositions = collectPositions(first)
    const secondPositions = collectPositions(second)
    expect(firstPositions).toEqual(secondPositions)

    for (const [x, y] of Object.values(firstPositions)) {
      expect(Number.isFinite(x)).toBe(true)
      expect(Number.isFinite(y)).toBe(true)
    }

    const uniquePositions = new Set(
      Object.values(firstPositions).map(([x, y]) => `${x.toFixed(6)}:${y.toFixed(6)}`),
    )
    expect(uniquePositions.size).toBeGreaterThan(1)
  })
})
