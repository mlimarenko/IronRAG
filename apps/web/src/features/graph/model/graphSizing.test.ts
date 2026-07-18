import { describe, expect, test } from 'vitest'
import type { GraphNode } from '@/shared/types'
import { buildNodeSizer, emphasizeNodeSize } from './graphSizing'

function node(edgeCount: number, id = `n-${edgeCount}-${Math.round(edgeCount * 7)}`): GraphNode {
  return {
    id,
    label: `Node ${id}`,
    type: 'entity',
    edgeCount,
    properties: {},
    sourceDocumentIds: [],
  }
}

/** A graph of `count` nodes, all isolated except one unique hub, so the sizer's
 *  produced min/max realise the tier floor and ceiling exactly. */
function tierProbe(count: number): { min: number; max: number } {
  const nodes = Array.from({ length: count }, (_, i) => node(i === 0 ? count + 5 : 0, `p${i}`))
  const sizer = buildNodeSizer(nodes)
  const sizes = nodes.map((n) => sizer(n))
  return { min: Math.min(...sizes), max: Math.max(...sizes) }
}

describe('buildNodeSizer — pixel band shrinks with graph size', () => {
  test('small graph uses the widest band', () => {
    const { min, max } = tierProbe(100)
    expect(min).toBeCloseTo(3)
    expect(max).toBeCloseTo(13)
  })

  test('mid / large / dense graphs use progressively tighter bands', () => {
    expect(tierProbe(1000)).toMatchObject({ min: 2, max: 7 })
    expect(tierProbe(8000)).toMatchObject({ min: 1.4, max: 4 })
    expect(tierProbe(20000)).toMatchObject({ min: 1, max: 2.6 })
  })
})

describe('buildNodeSizer — connectivity encoding', () => {
  test('least-connected mass sits on the floor, hub reaches the ceiling', () => {
    const nodes = [node(0, 'a'), node(0, 'b'), node(0, 'c'), node(40, 'hub')]
    const sizer = buildNodeSizer(nodes)
    expect(sizer(node(0, 'a'))).toBeCloseTo(3) // tier.min for a tiny graph
    expect(sizer(node(40, 'hub'))).toBeCloseTo(13) // tier.max
  })

  test('spreads a heavy-tailed distribution instead of collapsing to min', () => {
    // Degrees 0..1 dominate; a few mid/high nodes. The old sqrt-on-raw formula
    // parked nearly all of these on the floor.
    const degrees = [0, 0, 0, 0, 0, 1, 1, 1, 2, 3, 5, 8, 21]
    const nodes = degrees.map((d, i) => node(d, `n${i}`))
    const sizer = buildNodeSizer(nodes)

    const sizes = nodes.map((n) => sizer(n))
    const distinct = new Set(sizes.map((s) => s.toFixed(4)))
    expect(distinct.size).toBeGreaterThanOrEqual(6) // real differentiation
    expect(Math.min(...sizes)).toBeCloseTo(3)
    expect(Math.max(...sizes)).toBeCloseTo(13)

    // Monotonic in degree: higher degree never yields a smaller radius.
    const byDegree = nodes.map((n) => ({ d: n.edgeCount, s: sizer(n) })).sort((l, r) => l.d - r.d)
    for (let i = 1; i < byDegree.length; i += 1) {
      expect(byDegree[i]!.s).toBeGreaterThanOrEqual(byDegree[i - 1]!.s - 1e-9)
    }
  })

  test('degenerate inputs never produce NaN', () => {
    expect(Number.isFinite(buildNodeSizer([])(node(0)))).toBe(true)
    const flat = [node(3, 'x'), node(3, 'y'), node(3, 'z')]
    const sizer = buildNodeSizer(flat)
    for (const n of flat) expect(Number.isFinite(sizer(n))).toBe(true)
  })
})

describe('emphasizeNodeSize', () => {
  test('applies a hard floor so a tiny dense-graph node still pops', () => {
    expect(emphasizeNodeSize(2, 'selected')).toBe(9)
    expect(emphasizeNodeSize(2, 'hovered')).toBe(11)
  })

  test('grows an already-large sparse-graph hub proportionally', () => {
    // A 13 px hub used to stay 13 on selection (Math.max(13, 9)); now it grows.
    expect(emphasizeNodeSize(13, 'selected')).toBeCloseTo(13 * 1.7)
    expect(emphasizeNodeSize(13, 'hovered')).toBeCloseTo(13 * 1.9)
  })
})
