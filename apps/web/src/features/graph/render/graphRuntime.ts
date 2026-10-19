import type { GraphLayoutType } from '../model/config'
import { streamGraphLayout, type LayoutStream } from '../workers/graphLayoutClient'
import {
  createCameraFlight,
  fitCamera,
  isWorldPointVisible,
  panCamera,
  positionsBounds,
  screenToWorld,
  worldToScreen,
  zoomCamera,
  type CameraState,
  type Viewport,
} from './camera'
import { computeNodeStates, focusNodeIndices } from './highlight'
import { loadCachedPositions, storeCachedPositions } from './positionCache'
import { FOCUS_NEIGHBOR_LABEL_CAP, selectViewportLabels, type LabelCandidate } from './labels'
import { nodeSizeScale, scaledNodeRadius, zoomInFactor } from './nodeScale'
import { buildSpatialGrid, pickNodeAt, type SpatialGrid } from './spatialGrid'
import { createGraphRenderer, type GraphRenderer } from './webglRenderer'
import type { GraphSceneModel } from './sceneModel'

/**
 * Imperative runtime behind the GraphCanvas component: owns the WebGL
 * renderer, the camera, the spatial index, the label overlay, and the layout
 * worker streams. React only forwards prop changes into it and receives
 * selection/busy callbacks out of it.
 *
 * The frame loop is demand-driven: a frame renders only while something is
 * animating (position tween, camera flight) or after an invalidation
 * (camera move, state change). An idle graph costs zero CPU.
 */

export interface GraphRuntimeCallbacks {
  onSelect(nodeId: string | null): void
  onLayoutBusyChange(busy: boolean): void
  onFirstFrame(): void
}

export interface HoverStats {
  count: number
  p50: number
  p95: number
  max: number
}

const LAYOUT_TWEEN_MS = 550
const SNAPSHOT_TWEEN_MS = 420
const CAMERA_FLIGHT_MS = 450
const LABEL_REFRESH_MS = 140
/** While the camera is moving or a node is focused, base edges draw capped
 *  at this count (strongest-first). Keeps camera motion fluid on integrated
 *  GPUs at the 650k-edge scale; the full set returns on the settle frame. */
const INTERACTION_EDGE_CAP = 60_000
const MOTION_SETTLE_MS = 180

function quantileOf(values: number[], percentile: number): number {
  if (values.length === 0) return 0
  const sorted = [...values].sort((left, right) => left - right)
  return sorted[Math.min(sorted.length - 1, Math.floor(percentile * sorted.length))]!
}

export class GraphRuntime {
  private readonly model: GraphSceneModel
  private readonly renderer: GraphRenderer
  private readonly callbacks: GraphRuntimeCallbacks
  private readonly glCanvas: HTMLCanvasElement
  private readonly labelCanvas: HTMLCanvasElement

  private viewport: Viewport = { width: 1, height: 1 }
  private devicePixelRatio = 1
  private camera: CameraState
  private fitRatio: number
  private grid: SpatialGrid
  /** Largest base marker radius: bounds the on-screen hit-test reach. */
  private readonly maxNodeRadius: number
  private readonly states: Uint8Array
  private hiddenIndices: ReadonlySet<number> = new Set()
  private hoverIndex = -1
  private selectedIndex = -1
  private userMovedCamera = false

  private frameHandle: number | null = null
  private renderPending = false
  private cameraFlight: {
    flight: ReturnType<typeof createCameraFlight>
    startedAt: number
  } | null = null
  private labelCandidates: LabelCandidate[] = []
  private labelsRefreshedAt = 0
  private labelsDirty = true

  private layoutStream: LayoutStream | null = null
  private layoutGeneration = 0
  private firstFramePainted = false
  private initialSettleMarked = false
  private readonly hoverLatencies: number[] = []

  private pointerState: {
    pointerId: number
    lastX: number
    lastY: number
    moved: boolean
  } | null = null

  /** Camera-motion window end (performance.now ms); edge LOD stays capped
   *  until the camera has been still for MOTION_SETTLE_MS. */
  private motionUntil = 0

  private disposed = false

  constructor(options: {
    model: GraphSceneModel
    glCanvas: HTMLCanvasElement
    labelCanvas: HTMLCanvasElement
    callbacks: GraphRuntimeCallbacks
  }) {
    this.model = options.model
    this.glCanvas = options.glCanvas
    this.labelCanvas = options.labelCanvas
    this.callbacks = options.callbacks
    const renderer = createGraphRenderer(options.glCanvas, options.model)
    if (!renderer) {
      throw new Error('webgl2-unavailable')
    }
    this.renderer = renderer
    let maxNodeRadius = 0
    for (const size of options.model.sizes) {
      if (size > maxNodeRadius) maxNodeRadius = size
    }
    this.maxNodeRadius = maxNodeRadius
    this.states = new Uint8Array(options.model.count)
    this.grid = buildSpatialGrid(options.model.seedPositions, options.model.count)
    const bounds = positionsBounds(options.model.seedPositions, options.model.count)
    this.camera = fitCamera(bounds, this.viewport)
    this.fitRatio = this.camera.ratio

    this.attachPointerHandlers()
    this.attachWindowHandlers()
  }

  // ------------------------------------------------------------------ sizing

  resize(width: number, height: number, devicePixelRatio: number): void {
    if (width <= 0 || height <= 0) return
    const wasDefaultViewport = this.viewport.width <= 1
    this.viewport = { width, height }
    this.devicePixelRatio = devicePixelRatio
    this.renderer.resize(width, height, devicePixelRatio)
    this.labelCanvas.width = Math.round(width * devicePixelRatio)
    this.labelCanvas.height = Math.round(height * devicePixelRatio)
    if (wasDefaultViewport) {
      // First real measurement: refit so the seed frame fills the canvas.
      this.camera = fitCamera(
        positionsBounds(this.renderer.targetPositions, this.model.count),
        this.viewport,
      )
      this.fitRatio = this.camera.ratio
    }
    this.labelsDirty = true
    this.invalidate()
  }

  // ------------------------------------------------------------- layout flow

  /**
   * Start or switch a layout. The persistent position cache is consulted
   * first (reopening a library skips computation entirely); on a miss the
   * worker stream tweens snapshots in and the final result is persisted.
   */
  applyLayout(layout: GraphLayoutType, phase: 'initial' | 'switch'): void {
    this.layoutStream?.cancel()
    this.layoutGeneration += 1
    const generation = this.layoutGeneration
    if (phase === 'switch') this.callbacks.onLayoutBusyChange(true)

    void loadCachedPositions(this.model.topologyKey, layout)
      .catch(() => null)
      .then((cached) => {
        if (this.disposed || generation !== this.layoutGeneration) return
        if (cached?.length === this.model.count * 2) {
          this.applyLayoutPositions(layout, phase, generation, cached, true, true)
          return
        }
        this.startLayoutStream(layout, phase, generation)
      })
  }

  private startLayoutStream(
    layout: GraphLayoutType,
    phase: 'initial' | 'switch',
    generation: number,
  ): void {
    let sawFirstSnapshot = false
    const stream = streamGraphLayout({
      model: this.model,
      layout,
      onPositions: (positions, final) => {
        if (this.disposed || generation !== this.layoutGeneration) return
        const isFirst = !sawFirstSnapshot
        sawFirstSnapshot = true
        this.applyLayoutPositions(layout, phase, generation, positions, final, isFirst)
      },
    })
    this.layoutStream = stream
    stream.done.catch(() => {
      if (generation !== this.layoutGeneration) return
      // Keep the current frame (seed or previous layout): it is still a
      // valid, readable placement.
      if (phase === 'switch') this.callbacks.onLayoutBusyChange(false)
    })
  }

  private applyLayoutPositions(
    layout: GraphLayoutType,
    phase: 'initial' | 'switch',
    generation: number,
    positions: Float32Array,
    final: boolean,
    isFirst: boolean,
  ): void {
    this.renderer.tweenToPositions(positions, isFirst ? LAYOUT_TWEEN_MS : SNAPSHOT_TWEEN_MS)
    this.grid = buildSpatialGrid(this.renderer.targetPositions, this.model.count)
    this.labelsDirty = true
    if (isFirst || (final && !this.userMovedCamera)) {
      this.flyToFit()
    }
    if (isFirst && phase === 'switch') {
      // The switch lands with the first snapshot's tween: mark + unbusy
      // once that transition completes.
      window.setTimeout(() => {
        if (generation !== this.layoutGeneration) return
        this.callbacks.onLayoutBusyChange(false)
        performance.mark('graph:layout-switch-settled')
      }, LAYOUT_TWEEN_MS)
    }
    if (final && phase === 'initial' && !this.initialSettleMarked) {
      window.setTimeout(() => {
        if (this.disposed || this.initialSettleMarked) return
        this.initialSettleMarked = true
        performance.mark('graph:layout-settled')
      }, SNAPSHOT_TWEEN_MS)
    }
    if (final) {
      // Persist (or LRU-touch, on a cache hit) the settled layout so the
      // next open of the same topology skips computation entirely.
      void storeCachedPositions(this.model.topologyKey, layout, positions).catch(() => undefined)
    }
    this.invalidate()
  }

  // ------------------------------------------------------------ interactions

  setHiddenIndices(hidden: ReadonlySet<number>): void {
    this.hiddenIndices = hidden
    if (this.hoverIndex >= 0 && hidden.has(this.hoverIndex)) this.hoverIndex = -1
    this.refreshStates()
  }

  setSelectedId(nodeId: string | null): void {
    const index = nodeId ? (this.model.indexById.get(nodeId) ?? -1) : -1
    if (index === this.selectedIndex) return
    this.selectedIndex = index
    this.refreshStates()
    if (index >= 0) this.flyToNodeIfHidden(index)
  }

  setEdgeDrawCount(count: number): void {
    this.renderer.setEdgeDrawCount(count)
    this.invalidate()
  }

  fitView(): void {
    this.userMovedCamera = false
    this.flyToFit()
  }

  hoverStats(): HoverStats {
    return {
      count: this.hoverLatencies.length,
      p50: quantileOf(this.hoverLatencies, 0.5),
      p95: quantileOf(this.hoverLatencies, 0.95),
      max: quantileOf(this.hoverLatencies, 1),
    }
  }

  screenPositionOfNode(index: number): { x: number; y: number } | null {
    if (index < 0 || index >= this.model.count) return null
    const positions = this.renderer.targetPositions
    const rect = this.glCanvas.getBoundingClientRect()
    const screen = worldToScreen(
      this.camera,
      this.viewport,
      positions[index * 2]!,
      positions[index * 2 + 1]!,
    )
    return { x: rect.left + screen.x, y: rect.top + screen.y }
  }

  dispose(): void {
    this.disposed = true
    this.layoutStream?.cancel()
    if (this.frameHandle != null) cancelAnimationFrame(this.frameHandle)
    this.detachPointerHandlers()
    this.detachWindowHandlers()
    this.renderer.dispose()
  }

  // ----------------------------------------------------------------- private

  private flyToFit(): void {
    const bounds = positionsBounds(this.renderer.targetPositions, this.model.count)
    const target = fitCamera(bounds, this.viewport)
    this.fitRatio = target.ratio
    this.cameraFlight = {
      flight: createCameraFlight(this.camera, target, CAMERA_FLIGHT_MS),
      startedAt: performance.now(),
    }
    this.invalidate()
  }

  private flyToNodeIfHidden(index: number): void {
    const positions = this.renderer.targetPositions
    const worldX = positions[index * 2]!
    const worldY = positions[index * 2 + 1]!
    if (isWorldPointVisible(this.camera, this.viewport, worldX, worldY)) return
    const target: CameraState = {
      centerX: worldX,
      centerY: worldY,
      ratio: Math.min(this.camera.ratio, this.fitRatio / 4),
    }
    this.cameraFlight = {
      flight: createCameraFlight(this.camera, target, CAMERA_FLIGHT_MS),
      startedAt: performance.now(),
    }
    this.invalidate()
  }

  private noteCameraMotion(): void {
    this.motionUntil = performance.now() + MOTION_SETTLE_MS
    this.refreshInteractionEdgeCap()
  }

  private neighborsOf(index: number, limit: number): number[] {
    if (index < 0 || limit <= 0) return []
    const start = this.model.neighborOffsets[index] ?? 0
    const end = this.model.neighborOffsets[index + 1] ?? start
    const stop = Math.min(end, start + limit)
    const neighbors: number[] = []
    for (let cursor = start; cursor < stop; cursor += 1) {
      neighbors.push(this.model.neighborIndices[cursor]!)
    }
    return neighbors
  }

  private refreshInteractionEdgeCap(): void {
    const focusActive = this.hoverIndex >= 0 || this.selectedIndex >= 0
    const motionActive = performance.now() < this.motionUntil
    this.renderer.setInteractionEdgeCap(focusActive || motionActive ? INTERACTION_EDGE_CAP : null)
  }

  private refreshStates(): void {
    computeNodeStates(this.states, {
      count: this.model.count,
      hoverIndex: this.hoverIndex,
      selectedIndex: this.selectedIndex,
      neighborOffsets: this.model.neighborOffsets,
      neighborIndices: this.model.neighborIndices,
      hiddenIndices: this.hiddenIndices,
    })
    this.renderer.setStates(this.states)
    this.renderer.setHighlightEdges(this.selectedIndex, this.hoverIndex)
    this.refreshInteractionEdgeCap()
    this.labelsDirty = true
    this.invalidate()
  }

  private setHover(index: number, eventTimeStamp: number): void {
    if (index === this.hoverIndex) return
    this.hoverIndex = index
    this.refreshStates()
    this.glCanvas.style.cursor = index >= 0 ? 'pointer' : 'default'
    this.hoverLatencies.push(performance.now() - eventTimeStamp)
    if (this.hoverLatencies.length > 500) this.hoverLatencies.splice(0, 250)
  }

  private pickAt(offsetX: number, offsetY: number): number {
    const world = screenToWorld(this.camera, this.viewport, offsetX, offsetY)
    const sizeScale = nodeSizeScale(zoomInFactor(this.fitRatio, this.camera.ratio))
    return pickNodeAt(this.grid, this.renderer.targetPositions, this.model.sizes, {
      worldX: world.x,
      worldY: world.y,
      worldTolerance: 8 * this.camera.ratio,
      worldPerPixel: this.camera.ratio,
      sizeScale,
      maxPickRadiusPx: scaledNodeRadius(this.maxNodeRadius, sizeScale),
      isPickable: (index) => !this.hiddenIndices.has(index),
    })
  }

  private readonly handlePointerDown = (event: PointerEvent): void => {
    if (event.button !== 0) return
    this.glCanvas.setPointerCapture(event.pointerId)
    this.pointerState = {
      pointerId: event.pointerId,
      lastX: event.offsetX,
      lastY: event.offsetY,
      moved: false,
    }
  }

  private readonly handlePointerMove = (event: PointerEvent): void => {
    if (this.pointerState?.pointerId === event.pointerId) {
      const deltaX = event.offsetX - this.pointerState.lastX
      const deltaY = event.offsetY - this.pointerState.lastY
      if (Math.abs(deltaX) + Math.abs(deltaY) > 0) {
        this.pointerState.moved ||= Math.abs(deltaX) + Math.abs(deltaY) > 3
        this.camera = panCamera(this.camera, deltaX, deltaY)
        this.userMovedCamera = true
        this.cameraFlight = null
        this.pointerState.lastX = event.offsetX
        this.pointerState.lastY = event.offsetY
        this.noteCameraMotion()
        this.invalidate()
      }
      return
    }
    this.setHover(this.pickAt(event.offsetX, event.offsetY), event.timeStamp)
  }

  private readonly handlePointerUp = (event: PointerEvent): void => {
    const pointer = this.pointerState
    this.pointerState = null
    if (pointer?.pointerId !== event.pointerId) return
    if (pointer.moved) return
    const hit = this.pickAt(event.offsetX, event.offsetY)
    this.callbacks.onSelect(hit >= 0 ? (this.model.ids[hit] ?? null) : null)
  }

  private readonly handlePointerLeave = (): void => {
    if (this.hoverIndex !== -1) {
      this.hoverIndex = -1
      this.refreshStates()
      this.glCanvas.style.cursor = 'default'
    }
  }

  private readonly handleWheel = (event: WheelEvent): void => {
    event.preventDefault()
    const factor = Math.exp(-event.deltaY * 0.0015)
    this.camera = zoomCamera(
      this.camera,
      this.viewport,
      event.offsetX,
      event.offsetY,
      factor,
      this.fitRatio,
    )
    this.userMovedCamera = true
    this.cameraFlight = null
    this.labelsDirty = true
    this.noteCameraMotion()
    this.invalidate()
  }

  private attachPointerHandlers(): void {
    this.glCanvas.addEventListener('pointerdown', this.handlePointerDown)
    this.glCanvas.addEventListener('pointermove', this.handlePointerMove)
    this.glCanvas.addEventListener('pointerup', this.handlePointerUp)
    this.glCanvas.addEventListener('pointerleave', this.handlePointerLeave)
    this.glCanvas.addEventListener('wheel', this.handleWheel, { passive: false })
  }

  private detachPointerHandlers(): void {
    this.glCanvas.removeEventListener('pointerdown', this.handlePointerDown)
    this.glCanvas.removeEventListener('pointermove', this.handlePointerMove)
    this.glCanvas.removeEventListener('pointerup', this.handlePointerUp)
    this.glCanvas.removeEventListener('pointerleave', this.handlePointerLeave)
    this.glCanvas.removeEventListener('wheel', this.handleWheel)
  }

  private readonly handleWindowScroll = (): void => {
    this.invalidate()
  }

  private attachWindowHandlers(): void {
    window.addEventListener('scroll', this.handleWindowScroll, { capture: true, passive: true })
    window.visualViewport?.addEventListener('scroll', this.handleWindowScroll, { passive: true })
    window.visualViewport?.addEventListener('resize', this.handleWindowScroll, { passive: true })
  }

  private detachWindowHandlers(): void {
    window.removeEventListener('scroll', this.handleWindowScroll, true)
    window.visualViewport?.removeEventListener('scroll', this.handleWindowScroll)
    window.visualViewport?.removeEventListener('resize', this.handleWindowScroll)
  }

  // ------------------------------------------------------------- frame loop

  private invalidate(): void {
    this.renderPending = true
    this.frameHandle ??= requestAnimationFrame(this.renderFrame)
  }

  private readonly renderFrame = (nowMs: number): void => {
    this.frameHandle = null
    if (this.disposed) return
    this.renderPending = false

    if (this.cameraFlight) {
      const progress = (nowMs - this.cameraFlight.startedAt) / this.cameraFlight.flight.durationMs
      this.camera = this.cameraFlight.flight.at(progress)
      if (progress >= 1) this.cameraFlight = null
      this.labelsDirty = true
    }

    const motionActive = nowMs < this.motionUntil
    if (!motionActive && this.motionUntil !== 0) {
      this.motionUntil = 0
      this.refreshInteractionEdgeCap()
    }
    const sizeScale = nodeSizeScale(zoomInFactor(this.fitRatio, this.camera.ratio))
    const tweenActive = this.renderer.render(this.camera, this.viewport, nowMs, sizeScale)
    this.drawLabels(nowMs, tweenActive)

    if (!this.firstFramePainted) {
      this.firstFramePainted = true
      this.callbacks.onFirstFrame()
    }

    if (tweenActive || this.cameraFlight || this.renderPending || motionActive) {
      this.frameHandle = requestAnimationFrame(this.renderFrame)
    }
  }

  /** Nearest non-transparent ancestor background: the label halo color. */
  private resolveLabelHalo(): string {
    let element: HTMLElement | null = this.labelCanvas
    while (element) {
      const background = getComputedStyle(element).backgroundColor
      if (background && background !== 'transparent' && background !== 'rgba(0, 0, 0, 0)') {
        return background
      }
      element = element.parentElement
    }
    return 'rgba(255, 255, 255, 0.85)'
  }

  private drawLabels(nowMs: number, tweenActive: boolean): void {
    const context = this.labelCanvas.getContext('2d')
    if (!context) return
    if ((this.labelsDirty || nowMs - this.labelsRefreshedAt > LABEL_REFRESH_MS) && !tweenActive) {
      const focusIndices = focusNodeIndices(this.selectedIndex, this.hoverIndex)
      const neighborIndices = focusIndices.flatMap((index) =>
        this.neighborsOf(index, FOCUS_NEIGHBOR_LABEL_CAP),
      )
      this.labelCandidates = selectViewportLabels({
        grid: this.grid,
        positions: this.renderer.targetPositions,
        degrees: this.model.degrees,
        labels: this.model.labels,
        camera: this.camera,
        viewport: this.viewport,
        fitRatio: this.fitRatio,
        focusIndices,
        neighborIndices,
        isVisible: (index) => !this.hiddenIndices.has(index),
      })
      this.labelsRefreshedAt = nowMs
      this.labelsDirty = false
    }

    const scale = this.devicePixelRatio
    context.setTransform(scale, 0, 0, scale, 0, 0)
    context.clearRect(0, 0, this.viewport.width, this.viewport.height)
    if (tweenActive) return
    const style = getComputedStyle(this.labelCanvas)
    context.font = '11px ' + (style.fontFamily || 'sans-serif')
    context.textBaseline = 'middle'
    const fill = style.color || '#1e293b'
    const positions = this.renderer.targetPositions
    // Halo in the canvas background color (theme-aware), and greedy overlap
    // culling so dense areas do not smear labels on top of each other.
    context.lineWidth = 3
    context.strokeStyle = this.resolveLabelHalo()
    const placed: Array<{ x: number; y: number; width: number }> = []
    const labelHeight = 13
    const sizeScale = nodeSizeScale(zoomInFactor(this.fitRatio, this.camera.ratio))
    for (const candidate of this.labelCandidates) {
      const screen = worldToScreen(
        this.camera,
        this.viewport,
        positions[candidate.index * 2]!,
        positions[candidate.index * 2 + 1]!,
      )
      if (screen.x < -80 || screen.x > this.viewport.width + 20) continue
      if (screen.y < -20 || screen.y > this.viewport.height + 20) continue
      const offset = scaledNodeRadius(this.model.sizes[candidate.index]!, sizeScale) + 4
      const labelX = screen.x + offset
      const width = context.measureText(candidate.text).width
      const overlaps = placed.some(
        (rect) =>
          labelX < rect.x + rect.width + 6 &&
          rect.x < labelX + width + 6 &&
          Math.abs(screen.y - rect.y) < labelHeight,
      )
      if (overlaps) continue
      placed.push({ x: labelX, y: screen.y, width })
      context.strokeText(candidate.text, labelX, screen.y)
      context.fillStyle = fill
      context.fillText(candidate.text, labelX, screen.y)
    }
  }
}
