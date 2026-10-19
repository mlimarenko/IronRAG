import { GRAPH_EDGE_COLORS } from '../model/config'
import { buildFocusEdgeVertices } from './highlight'
import type { GraphSceneModel } from './sceneModel'
import type { CameraState, Viewport } from './camera'
import { viewMatrix } from './camera'
import { EDGE_FOCUS_DIM, NODE_DIM_ALPHA } from './emphasis'
import { NODE_RADIUS_CAP_PX } from './nodeScale'

export { EDGE_FOCUS_DIM } from './emphasis'

/**
 * Thin WebGL2 renderer for the knowledge graph.
 *
 * Design constraints (all measured against the legacy Sigma pipeline):
 *   - Node positions live in RG32F textures, not vertex buffers. Layout
 *     changes upload one texture and animate `u_progress` between the "from"
 *     and "to" textures — the position tween runs entirely in the vertex
 *     shader, so a layout transition costs one uniform per frame.
 *   - Nodes draw as one instanced quad per node (index = gl_InstanceID);
 *     edges draw as one static buffer of endpoint indices resolved through
 *     the position texture. Pan/zoom only changes the view matrix: no
 *     per-node JavaScript runs on camera events.
 *   - Interaction state (active / neighbor / dimmed / hidden) is one byte
 *     per node in an R8UI texture; hover updates are an O(n) byte fill plus
 *     one texture upload, and the active node's complete edge set draws in a
 *     dedicated pass so even a capped edge buffer shows every neighbor link.
 *
 * Returns `null` where WebGL2 is unavailable (including jsdom in tests);
 * the canvas component renders a fallback notice in that case.
 */

const TEXTURE_WIDTH = 2048

/**
 * Off-screen clip with finite w. `w = 0` becomes Inf after the perspective
 * divide; GPUs then rasterize giant triangles (the square/triangle grid
 * seen while scrolling a 100k-node graph).
 */
export const CLIP_HIDDEN_GLSL = 'vec4(2.0, 2.0, 0.0, 1.0)'

/** Independent triangles, never a strip that can leak across instances. */
export const NODE_INSTANCE_PRIMITIVE = 'triangles' as const
export const NODE_INSTANCE_VERTEX_COUNT = 6

export const NODE_VERTEX_SHADER = `#version 300 es
precision highp float;
precision highp int;

uniform mat3 u_matrix;
uniform vec2 u_viewportCss;
uniform float u_sizeScale;
uniform float u_progress;
uniform highp sampler2D u_positionsFrom;
uniform highp sampler2D u_positionsTo;
uniform highp usampler2D u_states;

in vec2 a_corner;
in float a_size;
in vec4 a_color;

out vec2 v_corner;
out vec4 v_color;
flat out uint v_emphasis;

vec2 nodePosition(int node) {
  ivec2 texel = ivec2(node % ${TEXTURE_WIDTH}, node / ${TEXTURE_WIDTH});
  vec2 from = texelFetch(u_positionsFrom, texel, 0).xy;
  vec2 to = texelFetch(u_positionsTo, texel, 0).xy;
  return mix(from, to, u_progress);
}

void main() {
  int node = gl_InstanceID;
  uint state = texelFetch(u_states, ivec2(node % ${TEXTURE_WIDTH}, node / ${TEXTURE_WIDTH}), 0).r;
  if ((state & 4u) != 0u) {
    gl_Position = ${CLIP_HIDDEN_GLSL};
    return;
  }
  uint emphasis = state & 3u;
  // Mirrors scaledNodeRadius() in nodeScale.ts: grow with zoom, never shrink
  // a marker already larger than the cap. Picking and label offsets read the
  // same function, so the drawn disc and its hit target stay the same size.
  float size = max(a_size, min(a_size * u_sizeScale, ${NODE_RADIUS_CAP_PX.toFixed(1)}));
  if (emphasis == 3u) size = max(size * 1.7, 9.0);
  else if (emphasis == 2u) size = max(size * 1.4, 6.5);

  vec2 world = nodePosition(node);
  vec3 clip = u_matrix * vec3(world, 1.0);
  vec2 cornerClip = a_corner * size * vec2(2.0, -2.0) / u_viewportCss;
  gl_Position = vec4(clip.xy + cornerClip, 0.0, 1.0);
  v_corner = a_corner;
  float alpha = emphasis == 1u ? ${NODE_DIM_ALPHA.toFixed(2)} : 1.0;
  v_color = vec4(a_color.rgb, a_color.a * alpha);
  v_emphasis = emphasis;
}
`

const NODE_FRAGMENT_SHADER = `#version 300 es
precision mediump float;
precision highp int;

in vec2 v_corner;
in vec4 v_color;
flat in uint v_emphasis;
out vec4 outColor;

void main() {
  float dist = length(v_corner);
  float feather = fwidth(dist) * 1.4;
  float coverage = 1.0 - smoothstep(1.0 - feather, 1.0, dist);
  if (coverage <= 0.0) discard;
  vec3 color = v_color.rgb;
  if (v_emphasis == 3u && dist > 0.72) {
    color = mix(color, vec3(1.0), 0.85);
  }
  outColor = vec4(color, v_color.a * coverage);
}
`

export const EDGE_VERTEX_SHADER = `#version 300 es
precision highp float;
precision highp int;

uniform mat3 u_matrix;
uniform float u_progress;
uniform highp sampler2D u_positionsFrom;
uniform highp sampler2D u_positionsTo;
uniform highp usampler2D u_states;
uniform vec4 u_color;
uniform float u_dimFactor;

in vec3 a_edge;

out vec4 v_color;

vec2 nodePosition(int node) {
  ivec2 texel = ivec2(node % ${TEXTURE_WIDTH}, node / ${TEXTURE_WIDTH});
  vec2 from = texelFetch(u_positionsFrom, texel, 0).xy;
  vec2 to = texelFetch(u_positionsTo, texel, 0).xy;
  return mix(from, to, u_progress);
}

void main() {
  int source = int(a_edge.x);
  int target = int(a_edge.y);
  uint stateSource = texelFetch(u_states, ivec2(source % ${TEXTURE_WIDTH}, source / ${TEXTURE_WIDTH}), 0).r;
  uint stateTarget = texelFetch(u_states, ivec2(target % ${TEXTURE_WIDTH}, target / ${TEXTURE_WIDTH}), 0).r;
  if ((stateSource & 4u) != 0u || (stateTarget & 4u) != 0u) {
    gl_Position = ${CLIP_HIDDEN_GLSL};
    return;
  }
  int node = a_edge.z < 0.5 ? source : target;
  vec3 clip = u_matrix * vec3(nodePosition(node), 1.0);
  gl_Position = vec4(clip.xy, 0.0, 1.0);
  v_color = vec4(u_color.rgb, u_color.a * u_dimFactor);
}
`

const EDGE_FRAGMENT_SHADER = `#version 300 es
precision mediump float;

in vec4 v_color;
out vec4 outColor;

void main() {
  outColor = v_color;
}
`

function parseRgba(cssColor: string): [number, number, number, number] {
  const match = /rgba?\(([\d.]+),\s*([\d.]+),\s*([\d.]+)(?:,\s*([\d.]+))?\)/.exec(cssColor)
  if (!match) return [0.5, 0.5, 0.5, 0.6]
  return [
    Number(match[1]) / 255,
    Number(match[2]) / 255,
    Number(match[3]) / 255,
    match[4] === undefined ? 1 : Number(match[4]),
  ]
}

const EDGE_BASE_COLOR = parseRgba(GRAPH_EDGE_COLORS.regular)
const EDGE_SELECTED_HIGHLIGHT_COLOR = parseRgba(GRAPH_EDGE_COLORS.highlight)
const EDGE_HOVER_HIGHLIGHT_COLOR = parseRgba(GRAPH_EDGE_COLORS.hover)

export interface GraphRenderer {
  resize(widthCss: number, heightCss: number, devicePixelRatio: number): void
  /** Replace positions instantly (no transition). */
  setPositions(positions: Float32Array): void
  /** Animate from the currently shown positions to `positions`. */
  tweenToPositions(positions: Float32Array, durationMs: number): void
  /** Latest layout target positions (hit-testing, labels, camera fits). */
  readonly targetPositions: Float32Array
  setStates(states: Uint8Array): void
  /** Draw the strongest `count` edges of the sorted edge buffer. */
  setEdgeDrawCount(count: number): void
  /**
   * Temporary cap layered under `setEdgeDrawCount` while the camera is in
   * motion or a node is focused. `null` restores the full configured count.
   */
  setInteractionEdgeCap(cap: number | null): void
  /** Upload selected and hover ego-networks for the two-color highlight pass. */
  setHighlightEdges(selectedIndex: number, hoverIndex: number): void
  /**
   * Render one frame; returns true while a tween needs further frames.
   * `sizeScale` grows every marker with the camera's zoom-in factor so a
   * deeply zoomed view is not a field of specks (IRG-121).
   */
  render(camera: CameraState, viewportCss: Viewport, nowMs: number, sizeScale: number): boolean
  dispose(): void
}

function compileShader(gl: WebGL2RenderingContext, type: number, source: string): WebGLShader {
  const shader = gl.createShader(type)
  if (!shader) throw new Error('shader allocation failed')
  gl.shaderSource(shader, source)
  gl.compileShader(shader)
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    const log = gl.getShaderInfoLog(shader) ?? 'unknown shader error'
    gl.deleteShader(shader)
    throw new Error(log)
  }
  return shader
}

function linkProgram(gl: WebGL2RenderingContext, vertex: string, fragment: string): WebGLProgram {
  const program = gl.createProgram()
  if (!program) throw new Error('program allocation failed')
  const vertexShader = compileShader(gl, gl.VERTEX_SHADER, vertex)
  const fragmentShader = compileShader(gl, gl.FRAGMENT_SHADER, fragment)
  gl.attachShader(program, vertexShader)
  gl.attachShader(program, fragmentShader)
  gl.linkProgram(program)
  gl.deleteShader(vertexShader)
  gl.deleteShader(fragmentShader)
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    const log = gl.getProgramInfoLog(program) ?? 'unknown link error'
    gl.deleteProgram(program)
    throw new Error(log)
  }
  return program
}

export function createGraphRenderer(
  canvas: HTMLCanvasElement,
  model: GraphSceneModel,
): GraphRenderer | null {
  const gl = canvas.getContext('webgl2', {
    alpha: true,
    antialias: false,
    depth: false,
    stencil: false,
    premultipliedAlpha: false,
    preserveDrawingBuffer: false,
  })
  if (!gl) return null

  const textureHeight = Math.max(1, Math.ceil(model.count / TEXTURE_WIDTH))
  const texelCount = TEXTURE_WIDTH * textureHeight

  let nodeProgram: WebGLProgram
  let edgeProgram: WebGLProgram
  try {
    nodeProgram = linkProgram(gl, NODE_VERTEX_SHADER, NODE_FRAGMENT_SHADER)
    edgeProgram = linkProgram(gl, EDGE_VERTEX_SHADER, EDGE_FRAGMENT_SHADER)
  } catch {
    return null
  }

  // --- position + state textures -------------------------------------------------
  const paddedPositions = new Float32Array(texelCount * 2)
  const paddedStates = new Uint8Array(texelCount)

  const createTexture = (): WebGLTexture => {
    const texture = gl.createTexture()
    if (!texture) throw new Error('texture allocation failed')
    gl.bindTexture(gl.TEXTURE_2D, texture)
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST)
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST)
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE)
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE)
    return texture
  }

  const uploadPositions = (texture: WebGLTexture, positions: Float32Array): void => {
    paddedPositions.set(positions.subarray(0, model.count * 2))
    gl.bindTexture(gl.TEXTURE_2D, texture)
    gl.texImage2D(
      gl.TEXTURE_2D,
      0,
      gl.RG32F,
      TEXTURE_WIDTH,
      textureHeight,
      0,
      gl.RG,
      gl.FLOAT,
      paddedPositions,
    )
  }

  const positionsFromTexture = createTexture()
  const positionsToTexture = createTexture()
  const statesTexture = createTexture()
  gl.bindTexture(gl.TEXTURE_2D, statesTexture)
  gl.texImage2D(
    gl.TEXTURE_2D,
    0,
    gl.R8UI,
    TEXTURE_WIDTH,
    textureHeight,
    0,
    gl.RED_INTEGER,
    gl.UNSIGNED_BYTE,
    paddedStates,
  )

  const fromPositions = model.seedPositions.slice()
  const targetPositions = model.seedPositions.slice()
  uploadPositions(positionsFromTexture, fromPositions)
  uploadPositions(positionsToTexture, targetPositions)

  // --- static vertex data --------------------------------------------------------
  const quadBuffer = gl.createBuffer()
  gl.bindBuffer(gl.ARRAY_BUFFER, quadBuffer)
  gl.bufferData(
    gl.ARRAY_BUFFER,
    new Float32Array([-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1]),
    gl.STATIC_DRAW,
  )

  const sizeBuffer = gl.createBuffer()
  gl.bindBuffer(gl.ARRAY_BUFFER, sizeBuffer)
  gl.bufferData(gl.ARRAY_BUFFER, model.sizes, gl.STATIC_DRAW)

  const colorBuffer = gl.createBuffer()
  gl.bindBuffer(gl.ARRAY_BUFFER, colorBuffer)
  gl.bufferData(gl.ARRAY_BUFFER, model.colors, gl.STATIC_DRAW)

  const edgeVertices = new Float32Array(model.edgeCount * 6)
  for (let edge = 0; edge < model.edgeCount; edge += 1) {
    const source = model.edgeEndpoints[edge * 2]!
    const target = model.edgeEndpoints[edge * 2 + 1]!
    edgeVertices[edge * 6] = source
    edgeVertices[edge * 6 + 1] = target
    edgeVertices[edge * 6 + 2] = 0
    edgeVertices[edge * 6 + 3] = source
    edgeVertices[edge * 6 + 4] = target
    edgeVertices[edge * 6 + 5] = 1
  }
  const edgeBuffer = gl.createBuffer()
  gl.bindBuffer(gl.ARRAY_BUFFER, edgeBuffer)
  gl.bufferData(gl.ARRAY_BUFFER, edgeVertices, gl.STATIC_DRAW)

  const highlightBuffer = gl.createBuffer()
  let selectedHighlightVertexCount = 0
  let hoverHighlightVertexCount = 0

  // --- VAOs ---------------------------------------------------------------------
  const nodeVao = gl.createVertexArray()
  gl.bindVertexArray(nodeVao)
  const cornerLocation = gl.getAttribLocation(nodeProgram, 'a_corner')
  const sizeLocation = gl.getAttribLocation(nodeProgram, 'a_size')
  const nodeColorLocation = gl.getAttribLocation(nodeProgram, 'a_color')
  gl.bindBuffer(gl.ARRAY_BUFFER, quadBuffer)
  gl.enableVertexAttribArray(cornerLocation)
  gl.vertexAttribPointer(cornerLocation, 2, gl.FLOAT, false, 0, 0)
  gl.bindBuffer(gl.ARRAY_BUFFER, sizeBuffer)
  gl.enableVertexAttribArray(sizeLocation)
  gl.vertexAttribPointer(sizeLocation, 1, gl.FLOAT, false, 0, 0)
  gl.vertexAttribDivisor(sizeLocation, 1)
  gl.bindBuffer(gl.ARRAY_BUFFER, colorBuffer)
  gl.enableVertexAttribArray(nodeColorLocation)
  gl.vertexAttribPointer(nodeColorLocation, 4, gl.UNSIGNED_BYTE, true, 0, 0)
  gl.vertexAttribDivisor(nodeColorLocation, 1)

  const edgeVao = gl.createVertexArray()
  gl.bindVertexArray(edgeVao)
  const edgeLocation = gl.getAttribLocation(edgeProgram, 'a_edge')
  gl.bindBuffer(gl.ARRAY_BUFFER, edgeBuffer)
  gl.enableVertexAttribArray(edgeLocation)
  gl.vertexAttribPointer(edgeLocation, 3, gl.FLOAT, false, 0, 0)

  const highlightVao = gl.createVertexArray()
  gl.bindVertexArray(highlightVao)
  gl.bindBuffer(gl.ARRAY_BUFFER, highlightBuffer)
  gl.enableVertexAttribArray(edgeLocation)
  gl.vertexAttribPointer(edgeLocation, 3, gl.FLOAT, false, 0, 0)
  gl.bindVertexArray(null)

  // --- uniforms -------------------------------------------------------------------
  const nodeUniforms = {
    matrix: gl.getUniformLocation(nodeProgram, 'u_matrix'),
    viewportCss: gl.getUniformLocation(nodeProgram, 'u_viewportCss'),
    sizeScale: gl.getUniformLocation(nodeProgram, 'u_sizeScale'),
    progress: gl.getUniformLocation(nodeProgram, 'u_progress'),
    positionsFrom: gl.getUniformLocation(nodeProgram, 'u_positionsFrom'),
    positionsTo: gl.getUniformLocation(nodeProgram, 'u_positionsTo'),
    states: gl.getUniformLocation(nodeProgram, 'u_states'),
  }
  const edgeUniforms = {
    matrix: gl.getUniformLocation(edgeProgram, 'u_matrix'),
    progress: gl.getUniformLocation(edgeProgram, 'u_progress'),
    positionsFrom: gl.getUniformLocation(edgeProgram, 'u_positionsFrom'),
    positionsTo: gl.getUniformLocation(edgeProgram, 'u_positionsTo'),
    states: gl.getUniformLocation(edgeProgram, 'u_states'),
    color: gl.getUniformLocation(edgeProgram, 'u_color'),
    dimFactor: gl.getUniformLocation(edgeProgram, 'u_dimFactor'),
  }

  gl.disable(gl.DEPTH_TEST)
  gl.enable(gl.BLEND)
  gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA)

  let edgeDrawCount = model.edgeCount
  let interactionEdgeCap: number | null = null
  let focusActive = false
  let tween: { startedAt: number; durationMs: number } | null = null
  let disposed = false

  const bindCommonTextures = (): void => {
    gl.activeTexture(gl.TEXTURE0)
    gl.bindTexture(gl.TEXTURE_2D, positionsFromTexture)
    gl.activeTexture(gl.TEXTURE1)
    gl.bindTexture(gl.TEXTURE_2D, positionsToTexture)
    gl.activeTexture(gl.TEXTURE2)
    gl.bindTexture(gl.TEXTURE_2D, statesTexture)
  }

  const currentProgress = (nowMs: number): number => {
    if (!tween) return 1
    const progress = (nowMs - tween.startedAt) / tween.durationMs
    if (progress >= 1) {
      tween = null
      fromPositions.set(targetPositions)
      uploadPositions(positionsFromTexture, fromPositions)
      return 1
    }
    // Smoothstep easing keeps large-layout transitions readable.
    return progress * progress * (3 - 2 * progress)
  }

  // Selected and hovered neighbourhoods are two ranges of one buffer, drawn at
  // full brightness while the rest of the graph is dimmed.
  const drawHighlightRanges = (): void => {
    gl.bindVertexArray(highlightVao)
    gl.uniform1f(edgeUniforms.dimFactor, 1)
    if (selectedHighlightVertexCount > 0) {
      gl.uniform4fv(edgeUniforms.color, EDGE_SELECTED_HIGHLIGHT_COLOR)
      gl.drawArrays(gl.LINES, 0, selectedHighlightVertexCount)
    }
    if (hoverHighlightVertexCount > 0) {
      gl.uniform4fv(edgeUniforms.color, EDGE_HOVER_HIGHLIGHT_COLOR)
      gl.drawArrays(gl.LINES, selectedHighlightVertexCount, hoverHighlightVertexCount)
    }
  }

  const drawEdgePass = (matrix: Float32Array, progress: number): void => {
    const drawnEdgeCount =
      interactionEdgeCap == null ? edgeDrawCount : Math.min(edgeDrawCount, interactionEdgeCap)
    const highlightVertexCount = selectedHighlightVertexCount + hoverHighlightVertexCount
    if (drawnEdgeCount === 0 && highlightVertexCount === 0) return

    gl.useProgram(edgeProgram)
    gl.uniformMatrix3fv(edgeUniforms.matrix, false, matrix)
    gl.uniform1f(edgeUniforms.progress, progress)
    gl.uniform1i(edgeUniforms.positionsFrom, 0)
    gl.uniform1i(edgeUniforms.positionsTo, 1)
    gl.uniform1i(edgeUniforms.states, 2)

    if (drawnEdgeCount > 0) {
      gl.bindVertexArray(edgeVao)
      gl.uniform4fv(edgeUniforms.color, EDGE_BASE_COLOR)
      gl.uniform1f(edgeUniforms.dimFactor, focusActive ? EDGE_FOCUS_DIM : 1)
      gl.drawArrays(gl.LINES, 0, drawnEdgeCount * 2)
    }
    if (highlightVertexCount > 0) drawHighlightRanges()
  }

  const drawNodePass = (
    matrix: Float32Array,
    progress: number,
    viewportCss: Viewport,
    sizeScale: number,
  ): void => {
    if (model.count === 0) return
    gl.useProgram(nodeProgram)
    gl.uniformMatrix3fv(nodeUniforms.matrix, false, matrix)
    gl.uniform2f(nodeUniforms.viewportCss, viewportCss.width, viewportCss.height)
    gl.uniform1f(nodeUniforms.sizeScale, sizeScale)
    gl.uniform1f(nodeUniforms.progress, progress)
    gl.uniform1i(nodeUniforms.positionsFrom, 0)
    gl.uniform1i(nodeUniforms.positionsTo, 1)
    gl.uniform1i(nodeUniforms.states, 2)
    gl.bindVertexArray(nodeVao)
    gl.drawArraysInstanced(
      NODE_INSTANCE_PRIMITIVE === 'triangles' ? gl.TRIANGLES : gl.TRIANGLE_STRIP,
      0,
      NODE_INSTANCE_VERTEX_COUNT,
      model.count,
    )
  }

  return {
    targetPositions,

    resize(widthCss: number, heightCss: number, devicePixelRatio: number): void {
      canvas.width = Math.max(1, Math.round(widthCss * devicePixelRatio))
      canvas.height = Math.max(1, Math.round(heightCss * devicePixelRatio))
      gl.viewport(0, 0, canvas.width, canvas.height)
    },

    setPositions(positions: Float32Array): void {
      tween = null
      fromPositions.set(positions.subarray(0, model.count * 2))
      targetPositions.set(positions.subarray(0, model.count * 2))
      uploadPositions(positionsFromTexture, fromPositions)
      uploadPositions(positionsToTexture, targetPositions)
    },

    tweenToPositions(positions: Float32Array, durationMs: number): void {
      // Freeze the currently shown interpolation as the new "from" frame so
      // consecutive tweens (streamed force snapshots) chain smoothly.
      const shownProgress = currentProgress(performance.now())
      if (shownProgress < 1) {
        for (let index = 0; index < model.count * 2; index += 1) {
          fromPositions[index] =
            fromPositions[index]! +
            (targetPositions[index]! - fromPositions[index]!) * shownProgress
        }
      } else {
        fromPositions.set(targetPositions)
      }
      targetPositions.set(positions.subarray(0, model.count * 2))
      uploadPositions(positionsFromTexture, fromPositions)
      uploadPositions(positionsToTexture, targetPositions)
      tween = { startedAt: performance.now(), durationMs: Math.max(16, durationMs) }
    },

    setStates(states: Uint8Array): void {
      paddedStates.set(states.subarray(0, model.count))
      gl.bindTexture(gl.TEXTURE_2D, statesTexture)
      gl.texImage2D(
        gl.TEXTURE_2D,
        0,
        gl.R8UI,
        TEXTURE_WIDTH,
        textureHeight,
        0,
        gl.RED_INTEGER,
        gl.UNSIGNED_BYTE,
        paddedStates,
      )
    },

    setEdgeDrawCount(count: number): void {
      edgeDrawCount = Math.max(0, Math.min(model.edgeCount, count))
    },

    setInteractionEdgeCap(cap: number | null): void {
      interactionEdgeCap = cap
    },

    setHighlightEdges(selectedIndex: number, hoverIndex: number): void {
      if (selectedIndex < 0 && hoverIndex < 0) {
        focusActive = false
        selectedHighlightVertexCount = 0
        hoverHighlightVertexCount = 0
        return
      }
      focusActive = true
      const highlight = buildFocusEdgeVertices(
        selectedIndex,
        hoverIndex,
        model.neighborOffsets,
        model.neighborIndices,
      )
      gl.bindBuffer(gl.ARRAY_BUFFER, highlightBuffer)
      gl.bufferData(gl.ARRAY_BUFFER, highlight.vertices, gl.DYNAMIC_DRAW)
      selectedHighlightVertexCount = highlight.selectedVertexCount
      hoverHighlightVertexCount = highlight.hoverVertexCount
    },

    render(camera: CameraState, viewportCss: Viewport, nowMs: number, sizeScale: number): boolean {
      if (disposed) return false
      const progress = currentProgress(nowMs)
      const matrix = viewMatrix(camera, viewportCss)

      gl.clearColor(0, 0, 0, 0)
      gl.clear(gl.COLOR_BUFFER_BIT)
      bindCommonTextures()

      // A frame is three passes over the same matrices. Written inline they
      // read as one nested block whose shape says nothing about the drawing.
      drawEdgePass(matrix, progress)
      drawNodePass(matrix, progress, viewportCss, sizeScale)

      gl.bindVertexArray(null)
      return tween !== null
    },

    dispose(): void {
      disposed = true
      gl.deleteProgram(nodeProgram)
      gl.deleteProgram(edgeProgram)
      gl.deleteTexture(positionsFromTexture)
      gl.deleteTexture(positionsToTexture)
      gl.deleteTexture(statesTexture)
      gl.deleteBuffer(quadBuffer)
      gl.deleteBuffer(sizeBuffer)
      gl.deleteBuffer(colorBuffer)
      gl.deleteBuffer(edgeBuffer)
      gl.deleteBuffer(highlightBuffer)
      gl.deleteVertexArray(nodeVao)
      gl.deleteVertexArray(edgeVao)
      gl.deleteVertexArray(highlightVao)
    },
  }
}
