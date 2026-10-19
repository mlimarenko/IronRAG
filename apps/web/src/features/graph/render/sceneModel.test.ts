import { describe, expect, it } from 'vitest'

import type { GraphEdge, GraphNode } from '@/shared/types'
import { GRAPH_NODE_COLORS } from '../model/config'
import { buildSceneModel } from './sceneModel'

function node(id: string, type = 'concept'): GraphNode {
  return {
    id,
    label: `label-${id}`,
    type: type as GraphNode['type'],
    edgeCount: 0,
    properties: {},
    sourceDocumentIds: [],
  }
}

function edge(sourceId: string, targetId: string, weight = 1): GraphEdge {
  return { id: '', sourceId, targetId, label: '', weight }
}

describe('buildSceneModel', () => {
  it('deduplicates opposite-direction edges and merges their weights', () => {
    const model = buildSceneModel(
      [node('a'), node('b'), node('c')],
      [edge('a', 'b', 2), edge('b', 'a', 3), edge('b', 'c', 1)],
    )

    expect(model.edgeCount).toBe(2)
    const pairKey = (index: number) =>
      [model.edgeEndpoints[index * 2], model.edgeEndpoints[index * 2 + 1]].sort().join('-')
    const weightByPair = new Map(
      Array.from({ length: model.edgeCount }, (_, index) => [
        pairKey(index),
        model.edgeWeights[index],
      ]),
    )
    const aIndex = model.indexById.get('a')!
    const bIndex = model.indexById.get('b')!
    const cIndex = model.indexById.get('c')!
    expect(weightByPair.get([aIndex, bIndex].sort().join('-'))).toBe(5)
    expect(weightByPair.get([bIndex, cIndex].sort().join('-'))).toBe(1)
  })

  it('drops self-loops and edges to unknown node ids', () => {
    const model = buildSceneModel(
      [node('a'), node('b')],
      [edge('a', 'a'), edge('a', 'ghost'), edge('a', 'b')],
    )
    expect(model.edgeCount).toBe(1)
  })

  it('sorts the edge buffer by weight descending so a draw cap keeps the strongest links', () => {
    const model = buildSceneModel(
      [node('a'), node('b'), node('c'), node('d')],
      [edge('a', 'b', 1), edge('b', 'c', 9), edge('c', 'd', 4)],
    )
    const weights = Array.from(model.edgeWeights)
    expect(weights).toEqual([9, 4, 1])
  })

  it('builds a symmetric CSR adjacency with matching degrees', () => {
    const model = buildSceneModel(
      [node('a'), node('b'), node('c')],
      [edge('a', 'b'), edge('a', 'c')],
    )
    const aIndex = model.indexById.get('a')!
    const bIndex = model.indexById.get('b')!
    expect(model.degrees[aIndex]).toBe(2)
    expect(model.degrees[bIndex]).toBe(1)

    const neighborsOf = (index: number) =>
      Array.from(
        model.neighborIndices.slice(model.neighborOffsets[index], model.neighborOffsets[index + 1]),
      )
    expect(neighborsOf(bIndex)).toEqual([aIndex])
    expect(neighborsOf(aIndex)).toHaveLength(2)
  })

  it('produces deterministic finite seed positions distinct per node', () => {
    const build = () =>
      buildSceneModel(
        [node('a'), node('b'), node('c'), node('d')],
        [edge('a', 'b'), edge('c', 'd')],
      )
    const first = build()
    const second = build()
    expect(Array.from(first.seedPositions)).toEqual(Array.from(second.seedPositions))
    const seen = new Set<string>()
    for (let index = 0; index < first.count; index += 1) {
      const x = first.seedPositions[index * 2]!
      const y = first.seedPositions[index * 2 + 1]!
      expect(Number.isFinite(x)).toBe(true)
      expect(Number.isFinite(y)).toBe(true)
      seen.add(`${x}:${y}`)
    }
    expect(seen.size).toBe(first.count)
  })

  it('maps node types to the shared palette with the entity fallback', () => {
    const model = buildSceneModel([node('a', 'person'), node('b', 'mystery')], [])
    const hexToRgb = (hex: string) => {
      const value = Number.parseInt(hex.slice(1), 16)
      return [(value >> 16) & 0xff, (value >> 8) & 0xff, value & 0xff]
    }
    expect(Array.from(model.colors.slice(0, 3))).toEqual(hexToRgb(GRAPH_NODE_COLORS.person!))
    expect(Array.from(model.colors.slice(4, 7))).toEqual(hexToRgb(GRAPH_NODE_COLORS.entity!))
  })

  it('keeps the topology key stable for identical input and distinct across topologies', () => {
    const nodes = [node('a'), node('b')]
    const first = buildSceneModel(nodes, [edge('a', 'b')])
    const second = buildSceneModel(nodes, [edge('a', 'b')])
    const different = buildSceneModel(nodes, [])
    expect(first.topologyKey).toBe(second.topologyKey)
    expect(first.topologyKey).not.toBe(different.topologyKey)
  })

  it('reports the highest-degree node as the hub anchor', () => {
    const model = buildSceneModel(
      [node('a'), node('hub'), node('c'), node('d')],
      [edge('hub', 'a'), edge('hub', 'c'), edge('hub', 'd'), edge('a', 'c')],
    )
    expect(model.maxDegreeIndex).toBe(model.indexById.get('hub'))
  })
})
