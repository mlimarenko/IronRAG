import { describe, expect, it } from 'vitest'

import { buildSpatialGrid, cellRangeForBounds, pickNodeAt, queryViewportNodes } from './spatialGrid'

describe('spatial grid', () => {
  const positions = new Float32Array([0, 0, 100, 0, 100, 100, 0, 100, 50, 50])
  const sizes = new Float32Array([5, 5, 5, 5, 10])
  const grid = buildSpatialGrid(positions, 5)

  it('picks the node whose radius covers the query point', () => {
    expect(
      pickNodeAt(grid, positions, sizes, {
        worldX: 51,
        worldY: 49,
        worldTolerance: 2,
        worldPerPixel: 1,
        sizeScale: 1,
        maxPickRadiusPx: 10,
      }),
    ).toBe(4)
    expect(
      pickNodeAt(grid, positions, sizes, {
        worldX: 99,
        worldY: 1,
        worldTolerance: 3,
        worldPerPixel: 1,
        sizeScale: 1,
        maxPickRadiusPx: 10,
      }),
    ).toBe(1)
  })

  it('returns -1 when nothing is under the pointer', () => {
    expect(
      pickNodeAt(grid, positions, sizes, {
        worldX: 25,
        worldY: 25,
        worldTolerance: 2,
        worldPerPixel: 1,
        sizeScale: 1,
        maxPickRadiusPx: 10,
      }),
    ).toBe(-1)
  })

  it('skips nodes filtered out by the pickability predicate', () => {
    expect(
      pickNodeAt(grid, positions, sizes, {
        worldX: 50,
        worldY: 50,
        worldTolerance: 2,
        worldPerPixel: 1,
        sizeScale: 1,
        maxPickRadiusPx: 10,
        isPickable: (index) => index !== 4,
      }),
    ).toBe(-1)
  })

  it('prefers the nearest of several overlapping nodes', () => {
    const dense = new Float32Array([0, 0, 3, 0])
    const denseSizes = new Float32Array([10, 10])
    const denseGrid = buildSpatialGrid(dense, 2)
    expect(
      pickNodeAt(denseGrid, dense, denseSizes, {
        worldX: 2.4,
        worldY: 0,
        worldTolerance: 12,
        worldPerPixel: 1,
        sizeScale: 1,
        maxPickRadiusPx: 10,
      }),
    ).toBe(1)
  })

  it('widens the search box for a scaled marker so its rim stays reachable', () => {
    // A 2 px marker at scale 3 covers a point 5 px away; at scale 1 it does not.
    // The search box must widen beyond worldTolerance as well, or the rim of a
    // scaled marker stays unreachable.
    const marker = new Float32Array([0, 0])
    const markerSizes = new Float32Array([2])
    const markerGrid = buildSpatialGrid(marker, 1)
    const pick = (sizeScale: number, maxPickRadiusPx: number) =>
      pickNodeAt(markerGrid, marker, markerSizes, {
        worldX: 0,
        worldY: 5,
        worldTolerance: 2,
        worldPerPixel: 1,
        sizeScale,
        maxPickRadiusPx,
      })
    expect(pick(1, 2)).toBe(-1)
    expect(pick(3, 6)).toBe(0)
  })

  it('collects viewport nodes within bounds up to the limit', () => {
    const inside = queryViewportNodes(
      grid,
      positions,
      { minX: -10, minY: -10, maxX: 60, maxY: 60 },
      10,
    )
    expect(new Set(inside)).toEqual(new Set([0, 4]))
    const limited = queryViewportNodes(
      grid,
      positions,
      { minX: -10, minY: -10, maxX: 200, maxY: 200 },
      2,
    )
    expect(limited).toHaveLength(2)
  })

  it('handles an empty node set', () => {
    const empty = buildSpatialGrid(new Float32Array(0), 0)
    expect(
      pickNodeAt(empty, new Float32Array(0), new Float32Array(0), {
        worldX: 0,
        worldY: 0,
        worldTolerance: 1,
        worldPerPixel: 1,
        sizeScale: 1,
        maxPickRadiusPx: 0,
      }),
    ).toBe(-1)
  })
})

describe('cellRangeForBounds', () => {
  const positions = new Float32Array([0, 0, 100, 0, 100, 100, 0, 100, 50, 50])
  const grid = buildSpatialGrid(positions, 5)

  it('clamps a rectangle that starts before the grid origin to the first cell', () => {
    const range = cellRangeForBounds(grid, { minX: -1e6, minY: -1e6, maxX: 0, maxY: 0 })
    expect(range.minColumn).toBe(0)
    expect(range.minRow).toBe(0)
  })

  it('clamps a rectangle that runs past the far edge to the last cell', () => {
    const range = cellRangeForBounds(grid, { minX: 0, minY: 0, maxX: 1e6, maxY: 1e6 })
    expect(range.maxColumn).toBe(grid.columns - 1)
    expect(range.maxRow).toBe(grid.rows - 1)
    // The last cell index must stay inside cellOffsets, which has one entry
    // more than there are cells.
    expect(range.maxRow * grid.columns + range.maxColumn + 1).toBeLessThan(grid.cellOffsets.length)
  })

  it('yields an empty range when the rectangle is entirely past the far edge', () => {
    const range = cellRangeForBounds(grid, { minX: 1e6, minY: 1e6, maxX: 2e6, maxY: 2e6 })
    expect(range.minColumn).toBeGreaterThan(range.maxColumn)
    expect(range.minRow).toBeGreaterThan(range.maxRow)
  })

  it('yields an empty range when the rectangle is entirely before the origin', () => {
    const range = cellRangeForBounds(grid, { minX: -2e6, minY: -2e6, maxX: -1e6, maxY: -1e6 })
    expect(range.maxColumn).toBeLessThan(range.minColumn)
    expect(range.maxRow).toBeLessThan(range.minRow)
  })
})
