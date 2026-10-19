import { scaledNodeRadius } from './nodeScale'

/**
 * Uniform spatial grid over node positions for O(cell) hit-testing and
 * viewport queries. Rebuilt with counting sort (two O(n) passes, zero
 * allocations beyond the two index arrays), so refreshing it after a layout
 * settles costs a few milliseconds even at the 230k-node scale — and hover
 * hit-testing never scans the full node array.
 */
export interface SpatialGrid {
  cellSize: number
  columns: number
  rows: number
  minX: number
  minY: number
  /** CSR: start offset per cell, `columns * rows + 1` entries. */
  cellOffsets: Uint32Array
  /** Node indices grouped by cell. */
  cellNodes: Uint32Array
}

export function buildSpatialGrid(
  positions: Float32Array,
  count: number,
  targetPerCell = 8,
): SpatialGrid {
  let minX = Infinity
  let minY = Infinity
  let maxX = -Infinity
  let maxY = -Infinity
  for (let index = 0; index < count; index += 1) {
    const x = positions[index * 2]!
    const y = positions[index * 2 + 1]!
    if (x < minX) minX = x
    if (x > maxX) maxX = x
    if (y < minY) minY = y
    if (y > maxY) maxY = y
  }
  if (count === 0) {
    return {
      cellSize: 1,
      columns: 1,
      rows: 1,
      minX: 0,
      minY: 0,
      cellOffsets: new Uint32Array(2),
      cellNodes: new Uint32Array(0),
    }
  }

  const spanX = Math.max(1e-6, maxX - minX)
  const spanY = Math.max(1e-6, maxY - minY)
  const targetCells = Math.max(1, Math.ceil(count / targetPerCell))
  const cellSize = Math.max(1e-6, Math.sqrt((spanX * spanY) / targetCells))
  const columns = Math.max(1, Math.min(4096, Math.ceil(spanX / cellSize)))
  const rows = Math.max(1, Math.min(4096, Math.ceil(spanY / cellSize)))

  const cellOf = (index: number): number => {
    const column = Math.min(
      columns - 1,
      Math.max(0, Math.floor((positions[index * 2]! - minX) / cellSize)),
    )
    const row = Math.min(
      rows - 1,
      Math.max(0, Math.floor((positions[index * 2 + 1]! - minY) / cellSize)),
    )
    return row * columns + column
  }

  const cellOffsets = new Uint32Array(columns * rows + 1)
  for (let index = 0; index < count; index += 1) {
    cellOffsets[cellOf(index) + 1]! += 1
  }
  for (let cell = 0; cell < columns * rows; cell += 1) {
    cellOffsets[cell + 1]! += cellOffsets[cell]!
  }
  const cellNodes = new Uint32Array(count)
  const cursor = cellOffsets.slice(0, columns * rows)
  for (let index = 0; index < count; index += 1) {
    const cell = cellOf(index)
    cellNodes[cursor[cell]!] = index
    cursor[cell]! += 1
  }

  return { cellSize, columns, rows, minX, minY, cellOffsets, cellNodes }
}

export type WorldBounds = Readonly<{ minX: number; minY: number; maxX: number; maxY: number }>

/** Inclusive cell index range covering a world-space rectangle. */
export type CellRange = Readonly<{
  minColumn: number
  maxColumn: number
  minRow: number
  maxRow: number
}>

/**
 * Clamp a world-space rectangle onto the grid's cell indices.
 *
 * This is the only part of a spatial query that can be wrong without the
 * result looking wrong: an unclamped upper bound reads past the end of
 * `cellOffsets`, and an unclamped lower bound wraps to the previous row's
 * cells and reports nodes that are nowhere near the query. Both used to be
 * written out longhand in each query, twice, with different argument shapes.
 */
export function cellRangeForBounds(grid: SpatialGrid, bounds: WorldBounds): CellRange {
  const column = (worldX: number) => Math.floor((worldX - grid.minX) / grid.cellSize)
  const row = (worldY: number) => Math.floor((worldY - grid.minY) / grid.cellSize)
  return {
    minColumn: Math.max(0, column(bounds.minX)),
    maxColumn: Math.min(grid.columns - 1, column(bounds.maxX)),
    minRow: Math.max(0, row(bounds.minY)),
    maxRow: Math.min(grid.rows - 1, row(bounds.maxY)),
  }
}

/**
 * Visit every node stored in the cells of `range`. Returning `false` from
 * `visit` stops the scan early, which is how the viewport query enforces its
 * budget; a scan that always wants every node returns nothing.
 */
function forEachNodeInCells(
  grid: SpatialGrid,
  range: CellRange,
  visit: (node: number) => boolean | void,
): void {
  for (let row = range.minRow; row <= range.maxRow; row += 1) {
    for (let column = range.minColumn; column <= range.maxColumn; column += 1) {
      const cell = row * grid.columns + column
      const end = grid.cellOffsets[cell + 1]!
      for (let cursor = grid.cellOffsets[cell]!; cursor < end; cursor += 1) {
        if (visit(grid.cellNodes[cursor]!) === false) return
      }
    }
  }
}

export type PickQuery = Readonly<{
  worldX: number
  worldY: number
  worldTolerance: number
  worldPerPixel: number
  /** Marker growth multiplier applied by the renderer at the current zoom. */
  sizeScale: number
  /** Largest already-scaled marker radius across all nodes, in CSS px. */
  maxPickRadiusPx: number
  isPickable?: (index: number) => boolean
}>

/**
 * Nearest node whose visual radius (in world units) covers the query point,
 * with a tolerance radius for small nodes. Returns -1 when nothing is hit.
 * Both the cell search box and each node's hit radius follow the scaled
 * visual radius (`sizeScale`), so zoomed-in markers are as easy to hit as
 * they look; `isPickable` filters hidden nodes out of the hit test.
 */
export function pickNodeAt(
  grid: SpatialGrid,
  positions: Float32Array,
  sizes: Float32Array,
  query: PickQuery,
): number {
  const reach = Math.max(query.worldTolerance, query.maxPickRadiusPx * query.worldPerPixel)
  const range = cellRangeForBounds(grid, {
    minX: query.worldX - reach,
    minY: query.worldY - reach,
    maxX: query.worldX + reach,
    maxY: query.worldY + reach,
  })

  let best = -1
  let bestDistance = Infinity
  forEachNodeInCells(grid, range, (node) => {
    if (query.isPickable && !query.isPickable(node)) return
    const dx = positions[node * 2]! - query.worldX
    const dy = positions[node * 2 + 1]! - query.worldY
    const distance = Math.hypot(dx, dy)
    const hitRadius = Math.max(
      scaledNodeRadius(sizes[node]!, query.sizeScale) * query.worldPerPixel,
      query.worldTolerance,
    )
    if (distance <= hitRadius && distance < bestDistance) {
      best = node
      bestDistance = distance
    }
  })
  return best
}

/**
 * Collect node indices inside a world-space rectangle (used for zoom-level
 * label selection). Stops after `limit` nodes to bound the work on
 * fully-zoomed-out views.
 */
export function queryViewportNodes(
  grid: SpatialGrid,
  positions: Float32Array,
  bounds: WorldBounds,
  limit: number,
  accept?: (index: number) => boolean,
): number[] {
  const range = cellRangeForBounds(grid, bounds)
  const result: number[] = []
  forEachNodeInCells(grid, range, (node) => {
    const x = positions[node * 2]!
    const y = positions[node * 2 + 1]!
    if (x < bounds.minX || x > bounds.maxX || y < bounds.minY || y > bounds.maxY) return
    if (accept && !accept(node)) return
    result.push(node)
    return result.length < limit
  })
  return result
}
