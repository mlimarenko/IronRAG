/**
 * Graph camera: world-space center plus `ratio` (world units per CSS pixel).
 * Rendering consumes `viewMatrix` (world -> clip); interaction converts
 * between screen and world coordinates. Pan and zoom only produce a new
 * camera state — the renderer redraws from buffers already on the GPU, so a
 * camera move never touches per-node data.
 */
export interface CameraState {
  centerX: number
  centerY: number
  /** World units per CSS pixel; smaller = zoomed in. */
  ratio: number
}

export interface Viewport {
  width: number
  height: number
}

export interface WorldBounds {
  minX: number
  minY: number
  maxX: number
  maxY: number
}

const CAMERA_MIN_RATIO_SCALE = 0.002
export const CAMERA_MAX_RATIO_SCALE = 8

export function positionsBounds(positions: Float32Array, count: number): WorldBounds {
  if (count === 0) return { minX: -1, minY: -1, maxX: 1, maxY: 1 }
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
  return { minX, minY, maxX, maxY }
}

/** Camera that fits `bounds` into `viewport` with a relative padding. */
export function fitCamera(bounds: WorldBounds, viewport: Viewport, padding = 0.08): CameraState {
  const spanX = Math.max(1e-6, bounds.maxX - bounds.minX)
  const spanY = Math.max(1e-6, bounds.maxY - bounds.minY)
  const ratio = Math.max(
    spanX / Math.max(1, viewport.width * (1 - padding * 2)),
    spanY / Math.max(1, viewport.height * (1 - padding * 2)),
  )
  return {
    centerX: (bounds.minX + bounds.maxX) / 2,
    centerY: (bounds.minY + bounds.maxY) / 2,
    ratio,
  }
}

/** Column-major 3x3 matrix mapping world coordinates to clip space. */
export function viewMatrix(camera: CameraState, viewport: Viewport): Float32Array {
  const scaleX = 2 / (viewport.width * camera.ratio)
  const scaleY = -2 / (viewport.height * camera.ratio)
  return new Float32Array([
    scaleX,
    0,
    0,
    0,
    scaleY,
    0,
    -camera.centerX * scaleX,
    -camera.centerY * scaleY,
    1,
  ])
}

export function screenToWorld(
  camera: CameraState,
  viewport: Viewport,
  screenX: number,
  screenY: number,
): { x: number; y: number } {
  return {
    x: camera.centerX + (screenX - viewport.width / 2) * camera.ratio,
    y: camera.centerY + (screenY - viewport.height / 2) * camera.ratio,
  }
}

export function worldToScreen(
  camera: CameraState,
  viewport: Viewport,
  worldX: number,
  worldY: number,
): { x: number; y: number } {
  return {
    x: viewport.width / 2 + (worldX - camera.centerX) / camera.ratio,
    y: viewport.height / 2 + (worldY - camera.centerY) / camera.ratio,
  }
}

export function panCamera(camera: CameraState, deltaXPx: number, deltaYPx: number): CameraState {
  return {
    ...camera,
    centerX: camera.centerX - deltaXPx * camera.ratio,
    centerY: camera.centerY - deltaYPx * camera.ratio,
  }
}

/**
 * Zoom by `factor` (>1 zooms in) keeping the world point under the screen
 * anchor stationary. `fitRatio` bounds the zoom range relative to the
 * fitted view so users can neither lose the graph nor zoom into nothing.
 */
export function zoomCamera(
  camera: CameraState,
  viewport: Viewport,
  anchorX: number,
  anchorY: number,
  factor: number,
  fitRatio: number,
): CameraState {
  const minRatio = fitRatio * CAMERA_MIN_RATIO_SCALE
  const maxRatio = fitRatio * CAMERA_MAX_RATIO_SCALE
  const nextRatio = Math.min(maxRatio, Math.max(minRatio, camera.ratio / factor))
  const anchor = screenToWorld(camera, viewport, anchorX, anchorY)
  const scale = nextRatio / camera.ratio
  return {
    centerX: anchor.x - (anchor.x - camera.centerX) * scale,
    centerY: anchor.y - (anchor.y - camera.centerY) * scale,
    ratio: nextRatio,
  }
}

export interface CameraFlight {
  /** Camera state at `progress` in [0, 1]. */
  at(progress: number): CameraState
  durationMs: number
}

function easeInOutCubic(t: number): number {
  return t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2
}

/** Smooth camera flight (eased interpolation of center and log-ratio). */
export function createCameraFlight(
  from: CameraState,
  to: CameraState,
  durationMs = 450,
): CameraFlight {
  const fromLogRatio = Math.log(from.ratio)
  const toLogRatio = Math.log(to.ratio)
  return {
    durationMs,
    at(progress: number): CameraState {
      const eased = easeInOutCubic(Math.min(1, Math.max(0, progress)))
      return {
        centerX: from.centerX + (to.centerX - from.centerX) * eased,
        centerY: from.centerY + (to.centerY - from.centerY) * eased,
        ratio: Math.exp(fromLogRatio + (toLogRatio - fromLogRatio) * eased),
      }
    },
  }
}

/** True when the world point is inside the camera's current viewport. */
export function isWorldPointVisible(
  camera: CameraState,
  viewport: Viewport,
  worldX: number,
  worldY: number,
  marginPx = 24,
): boolean {
  const screen = worldToScreen(camera, viewport, worldX, worldY)
  return (
    screen.x >= marginPx &&
    screen.x <= viewport.width - marginPx &&
    screen.y >= marginPx &&
    screen.y <= viewport.height - marginPx
  )
}
