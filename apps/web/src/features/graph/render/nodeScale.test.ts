import { describe, expect, it } from 'vitest'

import {
  NODE_RADIUS_CAP_PX,
  NODE_SIZE_SCALE_CAP,
  nodeSizeScale,
  scaledNodeRadius,
  zoomInFactor,
} from './nodeScale'
import { NODE_VERTEX_SHADER } from './webglRenderer'

describe('node scale', () => {
  it('leaves the fitted and zoomed-out view unscaled', () => {
    expect(nodeSizeScale(0.25)).toBe(1)
    expect(nodeSizeScale(1)).toBe(1)
    for (const base of [1, 2.6, 3, 7, 13]) {
      expect(scaledNodeRadius(base, 1)).toBe(base)
    }
  })

  it('grows with the square root of the zoom-in factor up to the cap', () => {
    expect(nodeSizeScale(2)).toBeCloseTo(Math.SQRT2)
    expect(nodeSizeScale(4)).toBe(2)
    expect(nodeSizeScale(9)).toBe(3)
    expect(nodeSizeScale(16)).toBe(3)
    expect(nodeSizeScale(500)).toBe(NODE_SIZE_SCALE_CAP)
  })

  it('is monotone non-decreasing in the zoom-in factor', () => {
    const factors = [1, 1.5, 2, 4, 9, 16, 36, 500]
    for (let index = 1; index < factors.length; index += 1) {
      expect(nodeSizeScale(factors[index]!)).toBeGreaterThanOrEqual(
        nodeSizeScale(factors[index - 1]!),
      )
    }
  })

  it('caps a scaled radius without shrinking an already-large one', () => {
    expect(scaledNodeRadius(1, 3)).toBe(3)
    expect(scaledNodeRadius(2.6, 3)).toBeCloseTo(7.8)
    expect(scaledNodeRadius(3, 3)).toBe(NODE_RADIUS_CAP_PX)
    expect(scaledNodeRadius(13, 3)).toBe(13)
  })

  it('keeps every plain marker strictly below the emphasis floor', () => {
    for (const factor of [1, 4, 36, 500]) {
      const scale = nodeSizeScale(factor)
      for (const base of [1, 2.6, 3, 7, 8]) {
        expect(scaledNodeRadius(base, scale)).toBeLessThan(9)
      }
    }
  })

  it('derives the zoom-in factor from the fitted ratio', () => {
    expect(zoomInFactor(0.5, 0.5)).toBeCloseTo(1)
    expect(zoomInFactor(0.5, 0.25)).toBeCloseTo(2)
  })

  it('stays coupled to the node vertex shader', () => {
    expect(NODE_VERTEX_SHADER).toContain('uniform float u_sizeScale;')
    // The drawn disc applies the same law as scaledNodeRadius(), including
    // the cap constant, so the hit target and the label offset match it.
    expect(NODE_VERTEX_SHADER).toContain(
      `max(a_size, min(a_size * u_sizeScale, ${NODE_RADIUS_CAP_PX.toFixed(1)}))`,
    )
  })
})
