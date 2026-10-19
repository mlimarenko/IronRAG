import { describe, expect, it } from 'vitest'

import {
  CAMERA_MAX_RATIO_SCALE,
  createCameraFlight,
  fitCamera,
  isWorldPointVisible,
  panCamera,
  positionsBounds,
  screenToWorld,
  viewMatrix,
  worldToScreen,
  zoomCamera,
} from './camera'

const viewport = { width: 200, height: 100 }

describe('camera', () => {
  it('fits bounds into the viewport with every corner visible', () => {
    const camera = fitCamera({ minX: -50, minY: -20, maxX: 50, maxY: 20 }, viewport)
    for (const [x, y] of [
      [-50, -20],
      [50, 20],
      [-50, 20],
      [50, -20],
    ] as const) {
      const screen = worldToScreen(camera, viewport, x, y)
      expect(screen.x).toBeGreaterThanOrEqual(0)
      expect(screen.x).toBeLessThanOrEqual(viewport.width)
      expect(screen.y).toBeGreaterThanOrEqual(0)
      expect(screen.y).toBeLessThanOrEqual(viewport.height)
    }
  })

  it('round-trips screen and world coordinates', () => {
    const camera = { centerX: 12, centerY: -7, ratio: 0.5 }
    const world = screenToWorld(camera, viewport, 33, 77)
    const screen = worldToScreen(camera, viewport, world.x, world.y)
    expect(screen.x).toBeCloseTo(33)
    expect(screen.y).toBeCloseTo(77)
  })

  it('pans by screen pixels scaled through the ratio', () => {
    const camera = panCamera({ centerX: 0, centerY: 0, ratio: 2 }, 10, -5)
    expect(camera.centerX).toBe(-20)
    expect(camera.centerY).toBe(10)
  })

  it('keeps the world point under the cursor stationary while zooming', () => {
    const camera = { centerX: 0, centerY: 0, ratio: 1 }
    const anchor = { x: 150, y: 30 }
    const before = screenToWorld(camera, viewport, anchor.x, anchor.y)
    const zoomed = zoomCamera(camera, viewport, anchor.x, anchor.y, 2, 1)
    const after = screenToWorld(zoomed, viewport, anchor.x, anchor.y)
    expect(after.x).toBeCloseTo(before.x)
    expect(after.y).toBeCloseTo(before.y)
    expect(zoomed.ratio).toBeCloseTo(0.5)
  })

  it('clamps zoom-out to the fit-relative ceiling', () => {
    const camera = { centerX: 0, centerY: 0, ratio: 1 }
    const zoomedOut = zoomCamera(camera, viewport, 100, 50, 1e-6, 1)
    expect(zoomedOut.ratio).toBeLessThanOrEqual(CAMERA_MAX_RATIO_SCALE)
  })

  it('produces a view matrix that maps the camera center to clip origin', () => {
    const camera = { centerX: 40, centerY: -3, ratio: 0.7 }
    const matrix = viewMatrix(camera, viewport)
    const clipX = matrix[0]! * camera.centerX + matrix[6]!
    const clipY = matrix[4]! * camera.centerY + matrix[7]!
    expect(clipX).toBeCloseTo(0)
    expect(clipY).toBeCloseTo(0)
  })

  it('computes bounds over interleaved positions', () => {
    const bounds = positionsBounds(new Float32Array([1, 2, -3, 4, 5, -6]), 3)
    expect(bounds).toEqual({ minX: -3, minY: -6, maxX: 5, maxY: 4 })
  })

  it('interpolates camera flights monotonically to the target', () => {
    const flight = createCameraFlight(
      { centerX: 0, centerY: 0, ratio: 4 },
      { centerX: 100, centerY: 0, ratio: 1 },
    )
    expect(flight.at(0)).toEqual({ centerX: 0, centerY: 0, ratio: 4 })
    const end = flight.at(1)
    expect(end.centerX).toBeCloseTo(100)
    expect(end.ratio).toBeCloseTo(1)
    expect(flight.at(0.5).centerX).toBeGreaterThan(0)
  })

  it('reports world-point visibility against the viewport with margin', () => {
    const camera = { centerX: 0, centerY: 0, ratio: 1 }
    expect(isWorldPointVisible(camera, viewport, 0, 0)).toBe(true)
    expect(isWorldPointVisible(camera, viewport, 500, 0)).toBe(false)
  })
})
