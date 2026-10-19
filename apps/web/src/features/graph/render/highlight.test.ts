import { describe, expect, it } from 'vitest'

import {
  buildFocusEdgeVertices,
  computeNodeStates,
  hiddenIdsToIndices,
  NODE_EMPHASIS_ACTIVE,
  NODE_EMPHASIS_DIMMED,
  NODE_EMPHASIS_NEIGHBOR,
  NODE_EMPHASIS_NORMAL,
  NODE_STATE_HIDDEN_BIT,
} from './highlight'

// Star topology: node 0 connects to 1 and 2; node 3 is isolated.
const neighborOffsets = new Uint32Array([0, 2, 3, 4, 4])
const neighborIndices = new Uint32Array([1, 2, 0, 0])

function states(input: {
  hoverIndex?: number
  selectedIndex?: number
  hiddenIndices?: ReadonlySet<number>
}): Uint8Array {
  const result = new Uint8Array(4)
  computeNodeStates(result, {
    count: 4,
    hoverIndex: input.hoverIndex ?? -1,
    selectedIndex: input.selectedIndex ?? -1,
    neighborOffsets,
    neighborIndices,
    hiddenIndices: input.hiddenIndices ?? new Set(),
  })
  return result
}

describe('computeNodeStates', () => {
  it('leaves every node normal when nothing is active', () => {
    expect(Array.from(states({}))).toEqual([
      NODE_EMPHASIS_NORMAL,
      NODE_EMPHASIS_NORMAL,
      NODE_EMPHASIS_NORMAL,
      NODE_EMPHASIS_NORMAL,
    ])
  })

  it('marks the hovered node active, its neighbors, and dims the rest', () => {
    expect(Array.from(states({ hoverIndex: 0 }))).toEqual([
      NODE_EMPHASIS_ACTIVE,
      NODE_EMPHASIS_NEIGHBOR,
      NODE_EMPHASIS_NEIGHBOR,
      NODE_EMPHASIS_DIMMED,
    ])
  })

  it('keeps both ego-networks when hover and selection are different nodes', () => {
    const result = states({ hoverIndex: 3, selectedIndex: 0 })
    expect(result[0]).toBe(NODE_EMPHASIS_ACTIVE)
    expect(result[3]).toBe(NODE_EMPHASIS_ACTIVE)
    expect(result[1]).toBe(NODE_EMPHASIS_NEIGHBOR)
    expect(result[2]).toBe(NODE_EMPHASIS_NEIGHBOR)
  })

  it('keeps the selected neighborhood when hovering one of its neighbors', () => {
    const result = states({ hoverIndex: 1, selectedIndex: 0 })
    expect(result[0]).toBe(NODE_EMPHASIS_ACTIVE)
    expect(result[1]).toBe(NODE_EMPHASIS_ACTIVE)
    expect(result[2]).toBe(NODE_EMPHASIS_NEIGHBOR)
    expect(result[3]).toBe(NODE_EMPHASIS_DIMMED)
  })

  it('uses the selection as the active node when nothing is hovered', () => {
    const result = states({ selectedIndex: 2 })
    expect(result[2]).toBe(NODE_EMPHASIS_ACTIVE)
    expect(result[0]).toBe(NODE_EMPHASIS_NEIGHBOR)
    expect(result[1]).toBe(NODE_EMPHASIS_DIMMED)
  })

  it('sets the hidden bit on top of the emphasis value', () => {
    const result = states({ hoverIndex: 0, hiddenIndices: new Set([1, 3]) })
    expect(result[1]! & NODE_STATE_HIDDEN_BIT).toBe(NODE_STATE_HIDDEN_BIT)
    expect(result[3]! & NODE_STATE_HIDDEN_BIT).toBe(NODE_STATE_HIDDEN_BIT)
    expect(result[0]! & NODE_STATE_HIDDEN_BIT).toBe(0)
  })
})

describe('buildFocusEdgeVertices', () => {
  it('uploads selected and hover neighborhoods as separate draw ranges', () => {
    const highlight = buildFocusEdgeVertices(0, 1, neighborOffsets, neighborIndices)
    expect(highlight.selectedVertexCount).toBe(4)
    expect(highlight.hoverVertexCount).toBe(2)
    expect(highlight.vertices).toHaveLength(18)
  })

  it('uses a single hover range when the pointer is on the selected node', () => {
    const highlight = buildFocusEdgeVertices(0, 0, neighborOffsets, neighborIndices)
    expect(highlight.selectedVertexCount).toBe(0)
    expect(highlight.hoverVertexCount).toBe(4)
  })
})

describe('hiddenIdsToIndices', () => {
  it('translates known ids and drops unknown ones', () => {
    const indexById = new Map([
      ['a', 0],
      ['b', 1],
    ])
    expect(hiddenIdsToIndices(new Set(['b', 'ghost']), indexById)).toEqual(new Set([1]))
  })
})
