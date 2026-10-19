/**
 * Typed-array force simulation (ForceAtlas2-style forces, Barnes-Hut
 * repulsion) for the graph layout worker.
 *
 * The previous pipeline ran `graphology-layout-forceatlas2`, which reads and
 * writes attribute objects per node per pass — measured at ~13.6 s PER
 * ITERATION at the 230k-node / 650k-edge scale. This implementation keeps
 * every quantity in flat Float32/Int32 arrays and measures ~0.2 s per
 * iteration at the same scale, which is what makes progressive layout
 * refinement (seed frame first, physics streaming in afterwards) possible.
 *
 * The API is snapshot-driven: `step(iterations)` advances the simulation and
 * the caller decides when to copy `positions` out (e.g. post a transferable
 * snapshot to the main thread every few hundred milliseconds).
 */

export interface ForceSimInput {
  /** Node count. */
  count: number
  /** Interleaved seed positions `[x0, y0, x1, y1, ...]`; copied, not mutated. */
  seedPositions: Float32Array
  /** Edge endpoint node indices, `2 * edgeCount` entries. */
  edgeEndpoints: Uint32Array
  /** Edge weights (support counts); stronger links pull endpoints closer. */
  edgeWeights: Float32Array
}

export interface ForceSim {
  /** Interleaved node positions, updated in place by `step`. */
  readonly positions: Float32Array
  /** Advance the simulation. Returns elapsed milliseconds. */
  step(iterations: number): number
  /** Total iterations applied so far. */
  readonly iterationsDone: number
}

/**
 * Iteration budget by graph size: the simulation converges visually in fewer
 * passes on large graphs because forces are stronger relative to the layout
 * diameter (and every pass costs linearly more).
 */
export function forceIterationBudget(count: number): number {
  if (count > 150_000) return 40
  if (count > 50_000) return 60
  if (count > 5_000) return 120
  return 300
}

const THETA_SQUARED = 1.44
const REPULSION = 2
const GRAVITY = 0.05
const MAX_FORCE_PER_MASS = 100
const SPEED_START = 0.02
const SPEED_END = 0.004

/**
 * Bounding square of the seed positions, as the root quadtree cell.
 *
 * The `+ 1e-6` is not cosmetic: a graph whose nodes are all collinear (or all
 * identical) has zero extent on one axis, and a zero half-size root cell
 * cannot contain anything, so every insertion would fall out of the tree.
 */
export function positionBounds(
  positions: Float32Array,
  count: number,
): Readonly<{ centerX: number; centerY: number; halfSize: number }> {
  let minX = Infinity
  let maxX = -Infinity
  let minY = Infinity
  let maxY = -Infinity
  for (let node = 0; node < count; node += 1) {
    const x = positions[node * 2]!
    const y = positions[node * 2 + 1]!
    if (x < minX) minX = x
    if (x > maxX) maxX = x
    if (y < minY) minY = y
    if (y > maxY) maxY = y
  }
  return {
    centerX: (minX + maxX) / 2,
    centerY: (minY + maxY) / 2,
    halfSize: Math.max(maxX - minX, maxY - minY) / 2 + 1e-6,
  }
}

/**
 * The Barnes-Hut acceptance test: may this cell be summarised by its centre of
 * mass instead of being descended into? A leaf always may (it holds at most
 * one body); an internal cell may once it subtends a small enough angle.
 */
export function treatCellAsOneBody(
  pointIndex: number,
  cellSize: number,
  distanceSquared: number,
): boolean {
  if (pointIndex !== -1) return true
  return (cellSize * cellSize) / distanceSquared < THETA_SQUARED
}

export function createForceSim(input: ForceSimInput): ForceSim {
  const { count, edgeEndpoints, edgeWeights } = input
  const edgeCount = edgeWeights.length
  const positions = input.seedPositions.slice()
  const displacement = new Float32Array(count * 2)

  // Mass = 1 + degree (FA2 convention): hubs repel harder and move slower,
  // which stops dense neighborhoods from collapsing into a point.
  const mass = new Float32Array(count).fill(1)
  for (let edge = 0; edge < edgeCount; edge += 1) {
    mass[edgeEndpoints[edge * 2]!]! += 1
    mass[edgeEndpoints[edge * 2 + 1]!]! += 1
  }

  // Flat-array Barnes-Hut quadtree, rebuilt per iteration.
  const capacity = Math.max(16, count * 4)
  const childOf = new Int32Array(capacity * 4)
  const massX = new Float64Array(capacity)
  const massY = new Float64Array(capacity)
  const cellMass = new Float64Array(capacity)
  const pointOf = new Int32Array(capacity)
  const centerX = new Float64Array(capacity)
  const centerY = new Float64Array(capacity)
  const halfSize = new Float64Array(capacity)
  let cellCount = 0

  const newCell = (cx: number, cy: number, half: number): number => {
    const cell = cellCount
    cellCount += 1
    childOf.fill(-1, cell * 4, cell * 4 + 4)
    pointOf[cell] = -1
    massX[cell] = 0
    massY[cell] = 0
    cellMass[cell] = 0
    centerX[cell] = cx
    centerY[cell] = cy
    halfSize[cell] = half
    return cell
  }

  const insertIntoQuadrant = (cell: number, point: number): number => {
    const quadrant =
      (positions[point * 2]! >= centerX[cell]! ? 1 : 0) +
      (positions[point * 2 + 1]! >= centerY[cell]! ? 2 : 0)
    let child = childOf[cell * 4 + quadrant]!
    if (child === -1) {
      const half = halfSize[cell]! / 2
      child = newCell(
        centerX[cell]! + (quadrant & 1 ? half : -half),
        centerY[cell]! + (quadrant & 2 ? half : -half),
        half,
      )
      childOf[cell * 4 + quadrant] = child
    }
    return child
  }

  /** Push a node down the tree until it lands in a cell of its own. */
  const insertPoint = (node: number): void => {
    let cell = 0
    for (;;) {
      massX[cell]! += positions[node * 2]! * mass[node]!
      massY[cell]! += positions[node * 2 + 1]! * mass[node]!
      cellMass[cell]! += mass[node]!
      const isEmptyLeaf = childOf[cell * 4] === -1 && pointOf[cell] === -1
      if (isEmptyLeaf && cellMass[cell] === mass[node]) {
        pointOf[cell] = node
        return
      }
      if (halfSize[cell]! < 1e-9) return // coincident points: aggregate here
      const resident = pointOf[cell]!
      if (resident !== -1) {
        pointOf[cell] = -1
        const residentCell = insertIntoQuadrant(cell, resident)
        pointOf[residentCell] = resident
        massX[residentCell]! += positions[resident * 2]! * mass[resident]!
        massY[residentCell]! += positions[resident * 2 + 1]! * mass[resident]!
        cellMass[residentCell]! += mass[resident]!
      }
      const nextCell = insertIntoQuadrant(cell, node)
      if (pointOf[nextCell] === -1 && childOf[nextCell * 4] === -1 && cellMass[nextCell] === 0) {
        pointOf[nextCell] = node
        massX[nextCell]! += positions[node * 2]! * mass[node]!
        massY[nextCell]! += positions[node * 2 + 1]! * mass[node]!
        cellMass[nextCell] += mass[node]!
        return
      }
      cell = nextCell
    }
  }

  const buildTree = (): void => {
    const root = positionBounds(positions, count)
    cellCount = 0
    newCell(root.centerX, root.centerY, root.halfSize)
    for (let node = 0; node < count; node += 1) insertPoint(node)
  }

  const traversal = new Int32Array(1024)

  /** Push a cell's four children onto the traversal stack; returns the new top. */
  const pushChildren = (cell: number, stackTop: number): number => {
    let top = stackTop
    for (let quadrant = 0; quadrant < 4; quadrant += 1) {
      const child = childOf[cell * 4 + quadrant]!
      if (child !== -1 && top < traversal.length) {
        traversal[top] = child
        top += 1
      }
    }
    return top
  }

  /** Barnes-Hut walk for one node, accumulating into `displacement`. */
  const repelOne = (node: number): void => {
    const x = positions[node * 2]!
    const y = positions[node * 2 + 1]!
    let forceX = 0
    let forceY = 0
    let stackTop = 1
    traversal[0] = 0
    while (stackTop > 0) {
      stackTop -= 1
      const cell = traversal[stackTop]!
      const aggregateMass = cellMass[cell]!
      if (aggregateMass === 0) continue
      const dx = x - massX[cell]! / aggregateMass
      const dy = y - massY[cell]! / aggregateMass
      const distanceSquared = dx * dx + dy * dy
      const size = halfSize[cell]! * 2
      if (!treatCellAsOneBody(pointOf[cell]!, size, distanceSquared)) {
        stackTop = pushChildren(cell, stackTop)
        continue
      }
      if (pointOf[cell] === node) continue
      if (distanceSquared > 1e-12) {
        const force = (REPULSION * mass[node]! * aggregateMass) / distanceSquared
        forceX += dx * force
        forceY += dy * force
      }
    }
    displacement[node * 2]! += forceX
    displacement[node * 2 + 1]! += forceY
  }

  const applyRepulsion = (): void => {
    for (let node = 0; node < count; node += 1) repelOne(node)
  }

  const applyAttraction = (): void => {
    for (let edge = 0; edge < edgeCount; edge += 1) {
      const source = edgeEndpoints[edge * 2]!
      const target = edgeEndpoints[edge * 2 + 1]!
      const dx = positions[target * 2]! - positions[source * 2]!
      const dy = positions[target * 2 + 1]! - positions[source * 2 + 1]!
      const weight = edgeWeights[edge]!
      displacement[source * 2]! += dx * weight
      displacement[source * 2 + 1]! += dy * weight
      displacement[target * 2]! -= dx * weight
      displacement[target * 2 + 1]! -= dy * weight
    }
  }

  const applyGravityAndIntegrate = (speed: number): void => {
    for (let node = 0; node < count; node += 1) {
      let forceX = displacement[node * 2]! - positions[node * 2]! * GRAVITY * mass[node]!
      let forceY = displacement[node * 2 + 1]! - positions[node * 2 + 1]! * GRAVITY * mass[node]!
      const magnitude = Math.hypot(forceX, forceY)
      const limit = MAX_FORCE_PER_MASS * mass[node]!
      if (magnitude > limit) {
        forceX = (forceX / magnitude) * limit
        forceY = (forceY / magnitude) * limit
      }
      positions[node * 2]! += (forceX / mass[node]!) * speed
      positions[node * 2 + 1]! += (forceY / mass[node]!) * speed
      displacement[node * 2] = 0
      displacement[node * 2 + 1] = 0
    }
  }

  let iterationsDone = 0
  const budget = forceIterationBudget(count)

  return {
    positions,
    get iterationsDone() {
      return iterationsDone
    },
    step(iterations: number): number {
      const startedAt = performance.now()
      for (let pass = 0; pass < iterations; pass += 1) {
        // Cooling: interpolate the integration speed across the budget so
        // early passes untangle quickly and late passes settle without jitter.
        const progress = Math.min(1, iterationsDone / budget)
        const speed = SPEED_START + (SPEED_END - SPEED_START) * progress
        buildTree()
        applyRepulsion()
        applyAttraction()
        applyGravityAndIntegrate(speed)
        iterationsDone += 1
      }
      return performance.now() - startedAt
    },
  }
}
