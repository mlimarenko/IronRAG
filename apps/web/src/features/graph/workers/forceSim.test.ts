import { describe, expect, it } from 'vitest'

import {
  createForceSim,
  forceIterationBudget,
  positionBounds,
  treatCellAsOneBody,
  type ForceSimInput,
} from './forceSim'

/** Two tight clusters (0-1-2 and 3-4-5) with no links between them. */
function clusteredInput(): ForceSimInput {
  const seedPositions = new Float32Array(12)
  for (let index = 0; index < 6; index += 1) {
    const angle = index * 2.399963
    seedPositions[index * 2] = Math.cos(angle) * (index + 1) * 5
    seedPositions[index * 2 + 1] = Math.sin(angle) * (index + 1) * 5
  }
  return {
    count: 6,
    seedPositions,
    edgeEndpoints: new Uint32Array([0, 1, 1, 2, 0, 2, 3, 4, 4, 5, 3, 5]),
    edgeWeights: new Float32Array([1, 1, 1, 1, 1, 1]),
  }
}

function distance(positions: Float32Array, a: number, b: number): number {
  return Math.hypot(
    positions[a * 2]! - positions[b * 2]!,
    positions[a * 2 + 1]! - positions[b * 2 + 1]!,
  )
}

describe('force simulation', () => {
  it('keeps every position finite across many iterations', () => {
    const sim = createForceSim(clusteredInput())
    sim.step(50)
    for (const value of sim.positions) {
      expect(Number.isFinite(value)).toBe(true)
    }
    expect(sim.iterationsDone).toBe(50)
  })

  it('is deterministic for identical input', () => {
    const first = createForceSim(clusteredInput())
    const second = createForceSim(clusteredInput())
    first.step(30)
    second.step(30)
    expect(Array.from(first.positions)).toEqual(Array.from(second.positions))
  })

  it('pulls connected nodes closer than unconnected ones', () => {
    const sim = createForceSim(clusteredInput())
    sim.step(120)
    const connected = distance(sim.positions, 0, 1)
    const crossCluster = distance(sim.positions, 0, 3)
    expect(connected).toBeLessThan(crossCluster)
  })

  it('contracts a distant connected pair much more than an identical unconnected pair', () => {
    // Two pairs at the same spread; only 0-1 is linked. Any attraction
    // regression (dropped or one-sided) loses the contraction asymmetry.
    const input: ForceSimInput = {
      count: 4,
      seedPositions: new Float32Array([-100, 0, 100, 0, 0, -100, 0, 100]),
      edgeEndpoints: new Uint32Array([0, 1]),
      edgeWeights: new Float32Array([1]),
    }
    const sim = createForceSim(input)
    sim.step(100)
    const connectedPair = distance(sim.positions, 0, 1)
    const unconnectedPair = distance(sim.positions, 2, 3)
    expect(connectedPair).toBeLessThan(unconnectedPair * 0.55)
  })

  it('does not mutate the caller seed positions', () => {
    const input = clusteredInput()
    const seedCopy = Array.from(input.seedPositions)
    const sim = createForceSim(input)
    sim.step(10)
    expect(Array.from(input.seedPositions)).toEqual(seedCopy)
  })

  it('shrinks the iteration budget as the graph grows', () => {
    expect(forceIterationBudget(1_000)).toBeGreaterThan(forceIterationBudget(100_000))
    expect(forceIterationBudget(100_000)).toBeGreaterThan(forceIterationBudget(200_000))
  })
})

describe('positionBounds', () => {
  it('centres the root cell on the seed extent', () => {
    const positions = new Float32Array([0, 0, 10, 4])
    const bounds = positionBounds(positions, 2)
    expect(bounds.centerX).toBeCloseTo(5)
    expect(bounds.centerY).toBeCloseTo(2)
    expect(bounds.halfSize).toBeCloseTo(5, 4)
  })

  it('never returns a zero half-size, so a degenerate graph still has a root cell', () => {
    // Every node on one point, and every node on one line: both have zero
    // extent on at least one axis, and a zero-size root cell would swallow
    // every insertion.
    expect(positionBounds(new Float32Array([3, 3, 3, 3]), 2).halfSize).toBeGreaterThan(0)
    expect(positionBounds(new Float32Array([0, 1, 0, 9]), 2).halfSize).toBeGreaterThan(0)
    expect(positionBounds(new Float32Array([7, 7]), 1).halfSize).toBeGreaterThan(0)
  })
})

describe('treatCellAsOneBody', () => {
  it('always summarises a leaf', () => {
    expect(treatCellAsOneBody(5, 1e9, 1e-9)).toBe(true)
  })

  it('descends into a wide internal cell that is close by', () => {
    expect(treatCellAsOneBody(-1, 10, 1)).toBe(false)
  })

  it('summarises an internal cell once it is far enough away', () => {
    expect(treatCellAsOneBody(-1, 1, 1e6)).toBe(true)
  })
})
