// Bespoke WebGL program + state factories for the "all edges" overlay canvas.
//
// This layer draws the FULL visible edge set on its own GL canvas to bypass
// Sigma's hard edge render cap and its per-edge reprocess on move (both
// measured bottlenecks in Firefox on large graphs). Two variants behind one
// `AllEdgesLayerState` union:
//   - `indexed` (WebGL2): node positions live in an RGBA32F texture; each edge
//     vertex carries only endpoint indices and reads its position via
//     `texelFetch`, so moving a node updates a single 1x1 texel instead of a
//     full vertex re-upload.
//   - `coordinate` (WebGL1 fallback): absolute XY per edge vertex.
//
// Pure module-level factories — they close over nothing from React and return
// `null` under JSDOM (no GL) so unit tests skip them. Extracted verbatim from
// SigmaGraph.tsx.

export type AllEdgesCoordinateLayerState = {
  kind: 'coordinate'
  gl: WebGLRenderingContext | WebGL2RenderingContext
  program: WebGLProgram
  buffer: WebGLBuffer
  positionLocation: number
  matrixLocation: WebGLUniformLocation
  colorLocation: WebGLUniformLocation
}

export type AllEdgesIndexedLayerState = {
  kind: 'indexed'
  gl: WebGL2RenderingContext
  program: WebGLProgram
  edgeBuffer: WebGLBuffer
  positionTexture: WebGLTexture
  edgeDataLocation: number
  matrixLocation: WebGLUniformLocation
  colorLocation: WebGLUniformLocation
  positionTextureLocation: WebGLUniformLocation
  positionTextureWidthLocation: WebGLUniformLocation
  nodeIndexById: Map<string, number>
  positionTextureWidth: number
  positionTextureHeight: number
  positionTextureData: Float32Array
  scratchTexel: Float32Array
}

export type AllEdgesLayerState = AllEdgesCoordinateLayerState | AllEdgesIndexedLayerState

function isJsdom(): boolean {
  return typeof window !== 'undefined' && window.navigator.userAgent.toLowerCase().includes('jsdom')
}

function compileAllEdgesShader(
  gl: WebGLRenderingContext | WebGL2RenderingContext,
  type: number,
  source: string,
): WebGLShader | null {
  const shader = gl.createShader(type)
  if (!shader) return null
  gl.shaderSource(shader, source)
  gl.compileShader(shader)
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    gl.deleteShader(shader)
    return null
  }
  return shader
}

function deleteIndexedResources(
  gl: WebGL2RenderingContext,
  resources: {
    program: WebGLProgram | null
    edgeBuffer: WebGLBuffer | null
    positionTexture: WebGLTexture | null
  },
): void {
  if (resources.program) gl.deleteProgram(resources.program)
  if (resources.edgeBuffer) gl.deleteBuffer(resources.edgeBuffer)
  if (resources.positionTexture) gl.deleteTexture(resources.positionTexture)
}

function createIndexedAllEdgesLayerState(canvas: HTMLCanvasElement): AllEdgesLayerState | null {
  if (isJsdom()) {
    return null
  }

  const gl = canvas.getContext('webgl2', {
    alpha: true,
    antialias: false,
    depth: false,
    preserveDrawingBuffer: false,
    premultipliedAlpha: true,
    stencil: false,
  })
  if (!gl) return null
  const coordinateFallback = () => createCoordinateAllEdgesLayerState(canvas, gl)

  const vertexShader = compileAllEdgesShader(
    gl,
    gl.VERTEX_SHADER,
    `#version 300 es
      precision highp float;

      in vec3 a_edgeData;
      uniform mat3 u_matrix;
      uniform sampler2D u_positionTexture;
      uniform float u_positionTextureWidth;

      vec2 readPosition(float nodeIndex) {
        float texX = mod(nodeIndex, u_positionTextureWidth);
        float texY = floor(nodeIndex / u_positionTextureWidth);
        return texelFetch(u_positionTexture, ivec2(int(texX), int(texY)), 0).xy;
      }

      void main() {
        float endpointIndex = mix(a_edgeData.x, a_edgeData.y, step(0.5, a_edgeData.z));
        vec2 graphPosition = readPosition(endpointIndex);
        vec3 position = u_matrix * vec3(graphPosition, 1.0);
        gl_Position = vec4(position.xy, 0.0, 1.0);
      }
    `,
  )
  const fragmentShader = compileAllEdgesShader(
    gl,
    gl.FRAGMENT_SHADER,
    `#version 300 es
      precision mediump float;

      uniform vec4 u_color;
      out vec4 outColor;

      void main() {
        outColor = u_color;
      }
    `,
  )
  if (!vertexShader || !fragmentShader) {
    if (vertexShader) gl.deleteShader(vertexShader)
    if (fragmentShader) gl.deleteShader(fragmentShader)
    return coordinateFallback()
  }

  const program = gl.createProgram()
  const edgeBuffer = gl.createBuffer()
  const positionTexture = gl.createTexture()
  if (!program || !edgeBuffer || !positionTexture) {
    deleteIndexedResources(gl, { program, edgeBuffer, positionTexture })
    return coordinateFallback()
  }
  gl.attachShader(program, vertexShader)
  gl.attachShader(program, fragmentShader)
  gl.linkProgram(program)
  gl.deleteShader(vertexShader)
  gl.deleteShader(fragmentShader)
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    gl.deleteProgram(program)
    gl.deleteBuffer(edgeBuffer)
    gl.deleteTexture(positionTexture)
    return coordinateFallback()
  }

  const edgeDataLocation = gl.getAttribLocation(program, 'a_edgeData')
  const matrixLocation = gl.getUniformLocation(program, 'u_matrix')
  const colorLocation = gl.getUniformLocation(program, 'u_color')
  const positionTextureLocation = gl.getUniformLocation(program, 'u_positionTexture')
  const positionTextureWidthLocation = gl.getUniformLocation(program, 'u_positionTextureWidth')
  if (
    edgeDataLocation < 0 ||
    !matrixLocation ||
    !colorLocation ||
    !positionTextureLocation ||
    !positionTextureWidthLocation
  ) {
    gl.deleteProgram(program)
    gl.deleteBuffer(edgeBuffer)
    gl.deleteTexture(positionTexture)
    return coordinateFallback()
  }

  gl.bindTexture(gl.TEXTURE_2D, positionTexture)
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST)
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST)
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE)
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE)
  gl.disable(gl.DEPTH_TEST)
  gl.enable(gl.BLEND)
  gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA)

  return {
    kind: 'indexed',
    gl,
    program,
    edgeBuffer,
    positionTexture,
    edgeDataLocation,
    matrixLocation,
    colorLocation,
    positionTextureLocation,
    positionTextureWidthLocation,
    nodeIndexById: new Map(),
    positionTextureWidth: 1,
    positionTextureHeight: 1,
    positionTextureData: new Float32Array(4),
    scratchTexel: new Float32Array(4),
  }
}

export function createCoordinateAllEdgesLayerState(
  canvas: HTMLCanvasElement,
  existingGl?: WebGLRenderingContext | WebGL2RenderingContext,
): AllEdgesCoordinateLayerState | null {
  if (isJsdom()) {
    return null
  }

  const gl =
    existingGl ??
    canvas.getContext('webgl', {
      alpha: true,
      antialias: false,
      depth: false,
      preserveDrawingBuffer: false,
      premultipliedAlpha: true,
      stencil: false,
    })
  if (!gl) return null
  const isWebGl2 =
    typeof WebGL2RenderingContext !== 'undefined' && gl instanceof WebGL2RenderingContext

  const vertexShader = compileAllEdgesShader(
    gl,
    gl.VERTEX_SHADER,
    isWebGl2
      ? `#version 300 es
        precision highp float;

        in vec2 a_position;
        uniform mat3 u_matrix;

        void main() {
          vec3 position = u_matrix * vec3(a_position, 1.0);
          gl_Position = vec4(position.xy, 0.0, 1.0);
        }
      `
      : `
        attribute vec2 a_position;
        uniform mat3 u_matrix;

        void main() {
          vec3 position = u_matrix * vec3(a_position, 1.0);
          gl_Position = vec4(position.xy, 0.0, 1.0);
        }
      `,
  )
  const fragmentShader = compileAllEdgesShader(
    gl,
    gl.FRAGMENT_SHADER,
    isWebGl2
      ? `#version 300 es
        precision mediump float;

        uniform vec4 u_color;
        out vec4 outColor;

        void main() {
          outColor = u_color;
        }
      `
      : `
        precision mediump float;
        uniform vec4 u_color;

        void main() {
          gl_FragColor = u_color;
        }
      `,
  )
  if (!vertexShader || !fragmentShader) return null

  const program = gl.createProgram()
  const buffer = gl.createBuffer()
  if (!program || !buffer) return null
  gl.attachShader(program, vertexShader)
  gl.attachShader(program, fragmentShader)
  gl.linkProgram(program)
  gl.deleteShader(vertexShader)
  gl.deleteShader(fragmentShader)
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    gl.deleteProgram(program)
    gl.deleteBuffer(buffer)
    return null
  }

  const positionLocation = gl.getAttribLocation(program, 'a_position')
  const matrixLocation = gl.getUniformLocation(program, 'u_matrix')
  const colorLocation = gl.getUniformLocation(program, 'u_color')
  if (positionLocation < 0 || !matrixLocation || !colorLocation) {
    gl.deleteProgram(program)
    gl.deleteBuffer(buffer)
    return null
  }

  gl.disable(gl.DEPTH_TEST)
  gl.enable(gl.BLEND)
  gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA)

  return {
    kind: 'coordinate',
    gl,
    program,
    buffer,
    positionLocation,
    matrixLocation,
    colorLocation,
  }
}

export function createAllEdgesLayerState(canvas: HTMLCanvasElement): AllEdgesLayerState | null {
  return createIndexedAllEdgesLayerState(canvas) ?? createCoordinateAllEdgesLayerState(canvas)
}
