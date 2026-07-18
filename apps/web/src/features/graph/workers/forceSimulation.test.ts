import { describe, expect, test } from 'vitest'
import Graph from 'graphology'
import { applyGraphLayout } from '../model/layouts'
import { forceLayoutIterations, runForceSimulation } from './forceSimulation'

function addNode(graph: Graph, id: string): void {
  graph.addNode(id, { x: 0, y: 0, size: 3, nodeType: 'entity', label: id })
}

function distance(graph: Graph, a: string, b: string): number {
  const ax = graph.getNodeAttribute(a, 'x') as number
  const ay = graph.getNodeAttribute(a, 'y') as number
  const bx = graph.getNodeAttribute(b, 'x') as number
  const by = graph.getNodeAttribute(b, 'y') as number
  return Math.hypot(ax - bx, ay - by)
}

/** Two tightly-weighted pairs joined by a single weak bridge. */
function twoTightPairs(): Graph {
  const graph = new Graph()
  for (const id of ['a1', 'a2', 'b1', 'b2']) addNode(graph, id)
  graph.addEdge('a1', 'a2', { weight: 30 })
  graph.addEdge('b1', 'b2', { weight: 30 })
  graph.addEdge('a1', 'b1', { weight: 1 })
  return graph
}

describe('forceLayoutIterations', () => {
  test('scales down as the graph grows', () => {
    expect(forceLayoutIterations(100)).toBeGreaterThan(forceLayoutIterations(2000))
    expect(forceLayoutIterations(2000)).toBeGreaterThan(forceLayoutIterations(10000))
    expect(forceLayoutIterations(10000)).toBeGreaterThan(forceLayoutIterations(30000))
  })
})

describe('force layout — seed + weighted ForceAtlas2', () => {
  test('the geometric seed alone is already non-degenerate (not all at 0,0)', () => {
    const graph = twoTightPairs()
    applyGraphLayout(graph, 'force')
    const positions = graph.mapNodes((_id, attr) => `${attr.x as number},${attr.y as number}`)
    expect(new Set(positions).size).toBeGreaterThan(1)
  })

  test('seeding prevents NaN — every position is finite after the simulation', () => {
    const graph = twoTightPairs()
    applyGraphLayout(graph, 'force')
    runForceSimulation(graph)
    graph.forEachNode((_id, attr) => {
      expect(Number.isFinite(attr.x as number)).toBe(true)
      expect(Number.isFinite(attr.y as number)).toBe(true)
    })
  })

  test('edge weight shortens distance: tight pairs cluster, the weak bridge stretches', () => {
    const graph = twoTightPairs()
    applyGraphLayout(graph, 'force')
    runForceSimulation(graph)
    const bridge = distance(graph, 'a1', 'b1')
    expect(distance(graph, 'a1', 'a2')).toBeLessThan(bridge)
    expect(distance(graph, 'b1', 'b2')).toBeLessThan(bridge)
  })

  test('an empty graph is a no-op (no throw)', () => {
    const graph = new Graph()
    expect(() => runForceSimulation(graph)).not.toThrow()
  })
})
