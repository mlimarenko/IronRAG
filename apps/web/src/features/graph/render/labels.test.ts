import { describe, expect, it } from 'vitest'

import { labelBudget, selectViewportLabels } from './labels'
import { buildSpatialGrid } from './spatialGrid'
import type { CameraState, Viewport } from './camera'

describe('labelBudget', () => {
  it('grows the label budget as the user zooms in', () => {
    const overview = labelBudget(1)
    const mid = labelBudget(5)
    const close = labelBudget(30)
    expect(overview).toBeLessThan(mid)
    expect(mid).toBeLessThan(close)
  })
})

describe('selectViewportLabels', () => {
  // Nodes on a small line; node 1 is the highest-degree hub.
  const positions = new Float32Array([0, 0, 10, 0, 20, 0, 30, 0, 500, 500])
  const degrees = new Uint32Array([1, 9, 2, 1, 5])
  const labels = ['n0', 'hub', 'n2', 'n3', 'far-away']
  const grid = buildSpatialGrid(positions, 5)
  const viewport: Viewport = { width: 100, height: 100 }
  const camera: CameraState = { centerX: 15, centerY: 0, ratio: 1 }

  const select = (options?: { focusIndices?: readonly number[]; hidden?: ReadonlySet<number> }) =>
    selectViewportLabels({
      grid,
      positions,
      degrees,
      labels,
      camera,
      viewport,
      fitRatio: 1,
      focusIndices: options?.focusIndices ?? [],
      isVisible: (index) => !(options?.hidden?.has(index) ?? false),
    })

  it('labels the most connected visible nodes and skips off-viewport ones', () => {
    const selected = select()
    const texts = selected.map((candidate) => candidate.text)
    expect(texts).toContain('hub')
    expect(texts).not.toContain('far-away')
    const hubPosition = texts.indexOf('hub')
    expect(hubPosition).toBe(0)
  })

  it('always includes focus nodes first with their focus labels', () => {
    const selected = select({ focusIndices: [3, 0] })
    expect(selected[0]?.index).toBe(3)
    expect(selected[1]?.index).toBe(0)
  })

  it('pins neighbor labels ahead of higher-degree nodes that are not nearby', () => {
    const count = 20
    const positions = new Float32Array(count * 2)
    const degrees = new Uint32Array(count)
    const labels = Array.from({ length: count }, (_, index) => `n${index}`)
    for (let index = 0; index < count; index += 1) {
      positions[index * 2] = index
      positions[index * 2 + 1] = 0
      degrees[index] = 40 - index
    }
    degrees[3] = 0
    const grid = buildSpatialGrid(positions, count)
    const selected = selectViewportLabels({
      grid,
      positions,
      degrees,
      labels,
      camera: { centerX: 10, centerY: 0, ratio: 1 },
      viewport: { width: 400, height: 100 },
      fitRatio: 1,
      focusIndices: [0],
      neighborIndices: [3],
      isVisible: () => true,
    })
    expect(selected[0]?.index).toBe(0)
    expect(selected[1]?.index).toBe(3)
  })

  it('excludes hidden nodes', () => {
    const selected = select({ hidden: new Set([1]) })
    expect(selected.map((candidate) => candidate.index)).not.toContain(1)
  })
})
