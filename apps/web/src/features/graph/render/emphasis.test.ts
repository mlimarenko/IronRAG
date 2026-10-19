import { describe, expect, it } from 'vitest'
import {
  EDGE_FOCUS_DIM,
  MIN_READABLE_EDGE_DIM,
  MIN_READABLE_NODE_DIM_ALPHA,
  NODE_DIM_ALPHA,
} from './emphasis'
import {
  CLIP_HIDDEN_GLSL,
  EDGE_FOCUS_DIM as RENDERER_EDGE_DIM,
  EDGE_VERTEX_SHADER,
  NODE_INSTANCE_PRIMITIVE,
  NODE_INSTANCE_VERTEX_COUNT,
  NODE_VERTEX_SHADER,
} from './webglRenderer'

describe('graph emphasis readability', () => {
  it('keeps dimmed nodes and edges above the readable floor', () => {
    expect(NODE_DIM_ALPHA).toBeGreaterThanOrEqual(MIN_READABLE_NODE_DIM_ALPHA)
    expect(EDGE_FOCUS_DIM).toBeGreaterThanOrEqual(MIN_READABLE_EDGE_DIM)
  })

  it('bakes those floors into the shaders instead of a near-invisible hardcode', () => {
    expect(NODE_VERTEX_SHADER).toContain(NODE_DIM_ALPHA.toFixed(2))
    expect(RENDERER_EDGE_DIM).toBe(EDGE_FOCUS_DIM)
  })

  it('clips hidden nodes and edges with finite w instead of Inf triangles', () => {
    expect(CLIP_HIDDEN_GLSL).toMatch(/1\.0\s*\)/)
    expect(CLIP_HIDDEN_GLSL).not.toMatch(/0\.0\s*\)/)
    expect(NODE_VERTEX_SHADER).toContain(CLIP_HIDDEN_GLSL)
    expect(EDGE_VERTEX_SHADER).toContain(CLIP_HIDDEN_GLSL)
    expect(NODE_VERTEX_SHADER).not.toContain('vec4(2.0, 2.0, 2.0, 0.0)')
    expect(EDGE_VERTEX_SHADER).not.toContain('vec4(2.0, 2.0, 2.0, 0.0)')
  })

  it('draws each node as its own triangles so a strip cannot leak across instances', () => {
    expect(NODE_INSTANCE_PRIMITIVE).toBe('triangles')
    expect(NODE_INSTANCE_VERTEX_COUNT).toBe(6)
  })
})
