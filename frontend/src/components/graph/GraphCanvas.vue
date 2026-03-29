<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import Sigma from 'sigma'
import { DEFAULT_EDGE_PROGRAM_CLASSES, DEFAULT_NODE_PROGRAM_CLASSES } from 'sigma/settings'
import { animateNodes } from 'sigma/utils'
import { NodeBorderProgram } from '@sigma/node-border'
import { createEdgeCurveProgram } from '@sigma/edge-curve'
import type { MultiUndirectedGraph } from 'graphology'
import type { SigmaStageEventPayload } from 'sigma/types'
import {
  applyGraphVisualState,
  createGraphModel,
  ensureFinitePositions,
  fallbackPosition,
  type GraphCanvasEdgeAttributes,
  type GraphCanvasNodeAttributes,
} from './graphCanvasModel'
import type { GraphEdge, GraphLayoutMode, GraphNode, GraphNodeType } from 'src/models/ui/graph'

const props = defineProps<{
  nodes: GraphNode[]
  edges: GraphEdge[]
  filter: GraphNodeType | ''
  focusedNodeId: string | null
  layoutMode: GraphLayoutMode
  surfaceVersion: number
  showFilteredArtifacts?: boolean
}>()

const emit = defineEmits<{
  selectNode: [id: string]
  clearFocus: []
  ready: [controls: { fitViewport: () => void; zoomIn: () => void; zoomOut: () => void }]
  rendererState: [available: boolean]
}>()

const { t } = useI18n()

const canvasRef = ref<HTMLDivElement | null>(null)
const sigmaRef = shallowRef<Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes> | null>(null)
const graphRef = shallowRef<
  MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes> | null
>(null)
const pendingDragNodeId = ref<string | null>(null)
const pendingDragViewport = ref<{ x: number; y: number } | null>(null)
const draggedNodeId = ref<string | null>(null)
const hoveredNodeId = ref<string | null>(null)
const dragStartViewport = ref<{ x: number; y: number } | null>(null)
const dragMoved = ref(false)
const ignoreStageClickUntil = ref(0)
const suppressNodeSelectionUntil = ref(0)
const skipNextFocusViewportSync = ref(false)
const didInitialFit = ref(false)
const renderMode = ref<'sigma' | 'placeholder'>('sigma')
const webglContextCleanup = ref<(() => void) | null>(null)
const hoverCursorCleanup = ref<(() => void) | null>(null)
const webglUnavailable = ref(false)
let relayoutTimer: number | null = null
let cancelRelayoutAnimation: (() => void) | null = null
let hoverCursorFrame: number | null = null
const NODE_DRAG_THRESHOLD_PX = 7

function setStageNodeHover(active: boolean): void {
  canvasRef.value?.classList.toggle('is-node-hover', active)
}

function updateHoveredNode(nodeId: string | null): void {
  if (hoveredNodeId.value === nodeId) {
    return
  }
  hoveredNodeId.value = nodeId
  setStageNodeHover(Boolean(nodeId))
}

function clearHoveredNode(): void {
  if (hoverCursorFrame !== null) {
    window.cancelAnimationFrame(hoverCursorFrame)
    hoverCursorFrame = null
  }
  updateHoveredNode(null)
}

function clearPendingNodeDrag(
  sigma: Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes> | null,
): void {
  pendingDragNodeId.value = null
  pendingDragViewport.value = null
  if (sigma) {
    sigma.setSetting('enableCameraPanning', true)
  }
}

function startDraggingNode(
  sigma: Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  nodeId: string,
): void {
  draggedNodeId.value = nodeId
  dragMoved.value = true
  clearPendingNodeDrag(null)
  sigma.setCustomBBox(sigma.getBBox())
  updateHoveredNode(null)
  canvasRef.value?.classList.add('is-dragging')
}

const baseNodes = computed(() => {
  const filteredByType =
    props.filter === '' ? props.nodes : props.nodes.filter((node) => node.nodeType === props.filter)
  if (props.showFilteredArtifacts) {
    return filteredByType
  }
  return filteredByType.filter((node) => !node.filteredArtifact)
})

const baseEdges = computed(() =>
  props.showFilteredArtifacts ? props.edges : props.edges.filter((edge) => !edge.filteredArtifact),
)
const denseOverview = computed(
  () => !props.focusedNodeId && (baseNodes.value.length > 120 || baseEdges.value.length > 240),
)
const denseFocusedGraph = computed(
  () => Boolean(props.focusedNodeId) && (baseNodes.value.length > 120 || baseEdges.value.length > 240),
)

function supportsWebGL(): boolean {
  const canvas = document.createElement('canvas')
  return Boolean(canvas.getContext('webgl2') ?? canvas.getContext('webgl'))
}

type SigmaInternal = Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes> & {
  activeListeners?: {
    handleMove?: (...args: unknown[]) => void
    handleMoveBody?: (...args: unknown[]) => void
    handleClick?: (...args: unknown[]) => void
    handleRightClick?: (...args: unknown[]) => void
    handleDoubleClick?: (...args: unknown[]) => void
    handleWheel?: (...args: unknown[]) => void
    handleDown?: (...args: unknown[]) => void
    handleUp?: (...args: unknown[]) => void
    handleLeave?: (...args: unknown[]) => void
    handleEnter?: (...args: unknown[]) => void
  }
  pickingDownSizingRatio?: number
  pixelRatio?: number
  hoveredNode?: string | null
  hoveredEdge?: string | null
}

function resolveDenseOverviewPixelRatio(container: HTMLDivElement): number | null {
  const area = container.offsetWidth * container.offsetHeight
  if (area >= 7_000_000) {
    return 0.78
  }
  if (area >= 4_200_000) {
    return 0.88
  }
  return null
}

function withTemporaryDevicePixelRatio<T>(pixelRatio: number | null, task: () => T): T {
  if (
    pixelRatio === null ||
    !Number.isFinite(pixelRatio) ||
    Math.abs(pixelRatio - (window.devicePixelRatio ?? 1)) < 0.01
  ) {
    return task()
  }

  try {
    Object.defineProperty(window, 'devicePixelRatio', {
      configurable: true,
      get: () => pixelRatio,
    })
  } catch {
    return task()
  }

  try {
    return task()
  } finally {
    try {
      delete (window as Window & { devicePixelRatio?: number }).devicePixelRatio
    } catch {
      // Ignore restoration failures and fall back to the browser-provided value.
    }
  }
}

function disableSigmaInteractionListeners(
  sigma: Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
): void {
  const internalSigma = sigma as SigmaInternal
  const activeListeners = internalSigma.activeListeners
  const mouseCaptor = sigma.getMouseCaptor()
  const touchCaptor = sigma.getTouchCaptor()

  if (activeListeners?.handleMove) {
    mouseCaptor.removeListener('mousemove', activeListeners.handleMove)
    touchCaptor.removeListener('touchdown', activeListeners.handleMove)
    touchCaptor.removeListener('touchmove', activeListeners.handleMove)
  }

  if (activeListeners?.handleMoveBody) {
    mouseCaptor.removeListener('mousemovebody', activeListeners.handleMoveBody)
    touchCaptor.removeListener('touchmove', activeListeners.handleMoveBody)
  }

  if (activeListeners?.handleClick) {
    mouseCaptor.removeListener('click', activeListeners.handleClick)
    touchCaptor.removeListener('tap', activeListeners.handleClick)
  }

  if (activeListeners?.handleRightClick) {
    mouseCaptor.removeListener('rightClick', activeListeners.handleRightClick)
  }

  if (activeListeners?.handleDoubleClick) {
    mouseCaptor.removeListener('doubleClick', activeListeners.handleDoubleClick)
    touchCaptor.removeListener('doubletap', activeListeners.handleDoubleClick)
  }

  if (activeListeners?.handleWheel) {
    mouseCaptor.removeListener('wheel', activeListeners.handleWheel)
  }

  if (activeListeners?.handleDown) {
    mouseCaptor.removeListener('mousedown', activeListeners.handleDown)
    touchCaptor.removeListener('touchdown', activeListeners.handleDown)
  }

  if (activeListeners?.handleUp) {
    mouseCaptor.removeListener('mouseup', activeListeners.handleUp)
    touchCaptor.removeListener('touchup', activeListeners.handleUp)
  }

  if (activeListeners?.handleLeave) {
    mouseCaptor.removeListener('mouseleave', activeListeners.handleLeave)
  }

  if (activeListeners?.handleEnter) {
    mouseCaptor.removeListener('mouseenter', activeListeners.handleEnter)
  }

  if (activeListeners) {
    internalSigma.hoveredNode = null
    internalSigma.hoveredEdge = null
  }
}

function resolveViewportBBox(): { x: [number, number]; y: [number, number] } | null {
  const graph = graphRef.value
  const focusedNodeId = props.focusedNodeId
  if (!graph || !focusedNodeId || !graph.hasNode(focusedNodeId)) {
    return null
  }

  const scopedNodeIds = new Set<string>([focusedNodeId])
  graph.forEachEdge((_, _attributes, source, target) => {
    if (source === focusedNodeId) {
      scopedNodeIds.add(target)
    } else if (target === focusedNodeId) {
      scopedNodeIds.add(source)
    }
  })

  let minX = Number.POSITIVE_INFINITY
  let maxX = Number.NEGATIVE_INFINITY
  let minY = Number.POSITIVE_INFINITY
  let maxY = Number.NEGATIVE_INFINITY

  graph.forEachNode((nodeId, attributes) => {
    if (!scopedNodeIds.has(nodeId)) {
      return
    }
    minX = Math.min(minX, attributes.x)
    maxX = Math.max(maxX, attributes.x)
    minY = Math.min(minY, attributes.y)
    maxY = Math.max(maxY, attributes.y)
  })

  if (
    !Number.isFinite(minX) ||
    !Number.isFinite(maxX) ||
    !Number.isFinite(minY) ||
    !Number.isFinite(maxY)
  ) {
    return null
  }

  const focusedAttributes = graph.getNodeAttributes(focusedNodeId)
  const centerX = (minX + maxX) / 2
  const centerY = (minY + maxY) / 2
  const width = Math.max(0.56, maxX - minX)
  const height = Math.max(0.52, maxY - minY)
  const framedWidth = Math.max(1.38, width * (scopedNodeIds.size <= 6 ? 2.05 : 1.84))
  const framedHeight = Math.max(1.12, height * (scopedNodeIds.size <= 6 ? 1.92 : 1.72))
  const viewportWidth = canvasRef.value?.clientWidth ?? window.innerWidth

  if (!focusedNodeId) {
    return {
      x: [centerX - framedWidth / 2, centerX + framedWidth / 2],
      y: [centerY - framedHeight / 2, centerY + framedHeight / 2],
    }
  }

  const overlayPadding =
    viewportWidth >= 1320
      ? { left: 0.24, right: 0.24, top: 0.08, bottom: 0.08 }
      : viewportWidth >= 980
        ? { left: 0.48, right: 0.14, top: 0.08, bottom: 0.3 }
        : { left: 0.16, right: 0.16, top: 0.08, bottom: 0.12 }
  const focusBias =
    viewportWidth >= 1320
      ? { x: 0.18, y: 0.06 }
      : viewportWidth >= 980
        ? { x: 0.46, y: 0.1 }
        : { x: 0.18, y: 0.08 }
  const framedCenterX = centerX + (focusedAttributes.x - centerX) * focusBias.x
  const framedCenterY = centerY + (focusedAttributes.y - centerY) * focusBias.y

  return {
    x: [
      framedCenterX - framedWidth / 2 - framedWidth * overlayPadding.left,
      framedCenterX + framedWidth / 2 + framedWidth * overlayPadding.right,
    ],
    y: [
      framedCenterY - framedHeight / 2 - framedHeight * overlayPadding.top,
      framedCenterY + framedHeight / 2 + framedHeight * overlayPadding.bottom,
    ],
  }
}

function fitViewport(duration = 260): void {
  const sigma = sigmaRef.value
  if (!sigma) {
    return
  }

  sigma.setCustomBBox(resolveViewportBBox())
  void sigma.getCamera().animate({ x: 0.5, y: 0.5, ratio: 1.02, angle: 0 }, { duration })
}

function recoverInvalidNodePosition(
  error: unknown,
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes> | null =
    graphRef.value,
): boolean {
  if (!(error instanceof Error) || !graph) {
    return false
  }

  const match = /node "([^"]+)"/.exec(error.message)
  const nodeId = match?.[1]
  if (!nodeId || !graph.hasNode(nodeId)) {
    return false
  }

  const fallback = fallbackPosition(nodeId)
  const attributes = graph.getNodeAttributes(nodeId)
  graph.replaceNodeAttributes(nodeId, {
    ...attributes,
    x: fallback.x,
    y: fallback.y,
    size: Number.isFinite(attributes.size) ? attributes.size : 6.2,
    color: attributes.color,
    borderColor: attributes.borderColor,
    borderSize: Number.isFinite(attributes.borderSize) ? attributes.borderSize : 0.18,
  })
  return true
}

function createSigma(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  container: HTMLDivElement,
): Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes> {
  ensureFinitePositions(graph)
  const isDenseOverview = denseOverview.value
  const isDenseFocusedGraph = denseFocusedGraph.value
  const cappedPixelRatio = isDenseOverview ? resolveDenseOverviewPixelRatio(container) : null
  const nodeProgramClasses = isDenseOverview
    ? DEFAULT_NODE_PROGRAM_CLASSES
    : {
        ...DEFAULT_NODE_PROGRAM_CLASSES,
        default: NodeBorderProgram as never,
      }
  const edgeProgramClasses = {
    ...DEFAULT_EDGE_PROGRAM_CLASSES,
    curvedNoArrow: createEdgeCurveProgram(),
  }
  const settings = {
    allowInvalidContainer: true,
    defaultNodeType: isDenseOverview ? 'circle' : 'default',
    defaultEdgeType: 'curvedNoArrow',
    renderLabels: !isDenseOverview,
    renderEdgeLabels: false,
    hideEdgesOnMove: false,
    hideLabelsOnMove: true,
    enableEdgeEvents: false,
    antiAliasingFeather: isDenseOverview ? 0.82 : 1.15,
    labelDensity: isDenseOverview ? 0.03 : isDenseFocusedGraph ? 0.06 : 0.22,
    labelGridCellSize: isDenseOverview ? 160 : isDenseFocusedGraph ? 132 : 92,
    labelRenderedSizeThreshold: isDenseOverview ? 18.4 : isDenseFocusedGraph ? 18.2 : 12.9,
    labelSize: 12,
    minCameraRatio: 0.05,
    maxCameraRatio: 4,
    autoRescale: true,
    autoCenter: true,
    nodeProgramClasses,
    edgeProgramClasses,
  } satisfies ConstructorParameters<
    typeof Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>
  >[2]

  try {
    return withTemporaryDevicePixelRatio(cappedPixelRatio, () =>
      new Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>(
        graph,
        container,
        settings,
      ),
    )
  } catch (error) {
    if (recoverInvalidNodePosition(error, graph)) {
      return withTemporaryDevicePixelRatio(cappedPixelRatio, () =>
        new Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>(
          graph,
          container,
          settings,
        ),
      )
    }
    throw error
  }
}

function optimizeSigmaRuntime(
  sigma: Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  denseOverview: boolean,
): void {
  const internalSigma = sigma as SigmaInternal
  disableSigmaInteractionListeners(sigma)

  if (denseOverview) {
    internalSigma.pickingDownSizingRatio = Math.max(
      5,
      (internalSigma.pixelRatio ?? window.devicePixelRatio ?? 1) * 6.4,
    )
    return
  }

  internalSigma.pickingDownSizingRatio = Math.max(
    2,
    (internalSigma.pixelRatio ?? window.devicePixelRatio ?? 1) * 2,
  )
}

function safeRefreshPartial(
  partialGraph: {
    nodes?: string[]
    edges?: string[]
  },
  options?: { skipIndexation?: boolean },
): void {
  const sigma = sigmaRef.value
  if (!sigma) {
    return
  }

  const nodeIds = partialGraph.nodes ?? []
  const edgeIds = partialGraph.edges ?? []
  if (!nodeIds.length && !edgeIds.length) {
    return
  }

  if (graphRef.value) {
    ensureFinitePositions(graphRef.value)
  }

  try {
    sigma.refresh({
      partialGraph: {
        nodes: nodeIds,
        edges: edgeIds,
      },
      skipIndexation: options?.skipIndexation ?? false,
    })
  } catch (error) {
    if (recoverInvalidNodePosition(error)) {
      sigma.refresh({
        partialGraph: {
          nodes: nodeIds,
          edges: edgeIds,
        },
        skipIndexation: options?.skipIndexation ?? false,
      })
      return
    }
    throw error
  }
}

function safeRefreshAll(options?: { skipIndexation?: boolean }): void {
  const sigma = sigmaRef.value
  if (!sigma) {
    return
  }

  if (graphRef.value) {
    ensureFinitePositions(graphRef.value)
  }

  try {
    sigma.refresh({ skipIndexation: options?.skipIndexation ?? false })
  } catch (error) {
    if (recoverInvalidNodePosition(error)) {
      sigma.refresh({ skipIndexation: options?.skipIndexation ?? false })
      return
    }
    throw error
  }
}

function animateRelayoutNodes(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  positions: Record<string, { x: number; y: number }>,
  duration: number,
  sigma: Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
): () => void {
  const targets = Object.fromEntries(
    Object.entries(positions).filter(([nodeId]) => graph.hasNode(nodeId)),
  )

  if (Object.keys(targets).length === 0) {
    return () => undefined
  }

  let cancelled = false
  let refreshFrameId: number | null = null
  const sigmaWithSchedule = sigma as Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes> & {
    scheduleRefresh?: (opts?: { skipIndexation?: boolean }) => void
  }

  const scheduleRenderFrame = () => {
    if (cancelled) {
      return
    }
    sigmaWithSchedule.scheduleRefresh?.()
    refreshFrameId = window.requestAnimationFrame(scheduleRenderFrame)
  }

  refreshFrameId = window.requestAnimationFrame(scheduleRenderFrame)

  const cancelNodeAnimation = animateNodes(
    graph,
    targets,
    { duration, easing: 'cubicInOut' },
    () => {
      cancelled = true
      if (refreshFrameId !== null) {
        window.cancelAnimationFrame(refreshFrameId)
        refreshFrameId = null
      }
      safeRefreshAll()
    },
  )

  return () => {
    cancelled = true
    if (refreshFrameId !== null) {
      window.cancelAnimationFrame(refreshFrameId)
    }
    cancelNodeAnimation()
  }
}

function zoomIn(): void {
  const sigma = sigmaRef.value
  if (!sigma) {
    return
  }
  void sigma.getCamera().animate(
    { ratio: Math.max(0.08, sigma.getCamera().ratio / 1.35) },
    { duration: 180 },
  )
}

function zoomOut(): void {
  const sigma = sigmaRef.value
  if (!sigma) {
    return
  }
  void sigma.getCamera().animate(
    { ratio: Math.min(4, sigma.getCamera().ratio * 1.35) },
    { duration: 180 },
  )
}

function destroyGraph(): void {
  if (cancelRelayoutAnimation) {
    cancelRelayoutAnimation()
    cancelRelayoutAnimation = null
  }
  if (relayoutTimer !== null) {
    window.clearTimeout(relayoutTimer)
    relayoutTimer = null
  }
  if (webglContextCleanup.value) {
    webglContextCleanup.value()
    webglContextCleanup.value = null
  }
  if (hoverCursorCleanup.value) {
    hoverCursorCleanup.value()
    hoverCursorCleanup.value = null
  }
  if (sigmaRef.value) {
    sigmaRef.value.kill()
    sigmaRef.value = null
  }
  graphRef.value = null
  pendingDragNodeId.value = null
  pendingDragViewport.value = null
  draggedNodeId.value = null
  dragStartViewport.value = null
  dragMoved.value = false
  clearHoveredNode()
  renderMode.value = 'sigma'
}

function registerWebglContextLossHandler(container: HTMLDivElement): void {
  if (webglContextCleanup.value) {
    webglContextCleanup.value()
    webglContextCleanup.value = null
  }

  const canvas = container.querySelector('canvas')
  if (!(canvas instanceof HTMLCanvasElement)) {
    return
  }

  const handleContextLost = (event: Event) => {
    event.preventDefault()
    webglUnavailable.value = true
    destroyGraph()
    renderMode.value = 'placeholder'
    emit('rendererState', false)
  }

  canvas.addEventListener('webglcontextlost', handleContextLost, false)
  webglContextCleanup.value = () => {
    canvas.removeEventListener('webglcontextlost', handleContextLost, false)
  }
}

function registerSigmaInteractions(
  sigma: Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
): void {
  const findNearestNodeAtViewportPoint = (x: number, y: number): string | null => {
    const graph = graphRef.value
    if (!graph) {
      return null
    }

    let nearestNodeId: string | null = null
    let nearestDistance = Number.POSITIVE_INFINITY

    graph.forEachNode((nodeId, attributes) => {
      const viewportPosition = sigma.graphToViewport({
        x: attributes.x,
        y: attributes.y,
      })
      const distance = Math.hypot(viewportPosition.x - x, viewportPosition.y - y)
      const hitRadius = Math.max(16, attributes.size * 1.9)

      if (distance <= hitRadius && distance < nearestDistance) {
        nearestDistance = distance
        nearestNodeId = nodeId
      }
    })

    return nearestNodeId
  }

  let latestHoverPoint: { x: number; y: number } | null = null

  const scheduleHoverSync = (x: number, y: number) => {
    if (draggedNodeId.value) {
      return
    }

    latestHoverPoint = { x, y }
    if (hoverCursorFrame !== null) {
      return
    }

    hoverCursorFrame = window.requestAnimationFrame(() => {
      hoverCursorFrame = null
      if (!latestHoverPoint || draggedNodeId.value) {
        return
      }
      updateHoveredNode(findNearestNodeAtViewportPoint(latestHoverPoint.x, latestHoverPoint.y))
    })
  }

  const resetHoverSync = () => {
    latestHoverPoint = null
    clearHoveredNode()
  }

  canvasRef.value?.addEventListener('mouseleave', resetHoverSync)
  hoverCursorCleanup.value = () => {
    canvasRef.value?.removeEventListener('mouseleave', resetHoverSync)
    resetHoverSync()
  }

  const graph = graphRef.value
  const mouseCaptor = sigma.getMouseCaptor()

  mouseCaptor.on('click', (event) => {
    if (
      Date.now() < ignoreStageClickUntil.value ||
      Date.now() < suppressNodeSelectionUntil.value ||
      dragMoved.value
    ) {
      return
    }

    const nearestNodeId = findNearestNodeAtViewportPoint(event.x, event.y)
    if (nearestNodeId) {
      ignoreStageClickUntil.value = Date.now() + 180
      skipNextFocusViewportSync.value = true
      emit('selectNode', nearestNodeId)
      return
    }

    emit('clearFocus')
  })

  mouseCaptor.on('mousedown', (event) => {
    const nearestNodeId = findNearestNodeAtViewportPoint(event.x, event.y)
    if (!nearestNodeId) {
      return
    }

    pendingDragNodeId.value = nearestNodeId
    pendingDragViewport.value = { x: event.x, y: event.y }
    dragStartViewport.value = { x: event.x, y: event.y }
    dragMoved.value = false
    sigma.setSetting('enableCameraPanning', false)
    event.preventSigmaDefault()
    event.original.preventDefault()
    event.original.stopPropagation()
  })

  mouseCaptor.on('mousemovebody', (event: SigmaStageEventPayload['event']) => {
    if (pendingDragNodeId.value && !draggedNodeId.value) {
      if (!graph?.hasNode(pendingDragNodeId.value)) {
        clearPendingNodeDrag(sigma)
        scheduleHoverSync(event.x, event.y)
        return
      }

      const pressViewport = pendingDragViewport.value
      if (pressViewport) {
        const distance = Math.hypot(event.x - pressViewport.x, event.y - pressViewport.y)
        if (distance >= NODE_DRAG_THRESHOLD_PX) {
          startDraggingNode(sigma, pendingDragNodeId.value)
        } else {
          return
        }
      }
    }

    if (!draggedNodeId.value || !graph?.hasNode(draggedNodeId.value)) {
      scheduleHoverSync(event.x, event.y)
      return
    }

    if (dragStartViewport.value) {
      const distance = Math.hypot(
        event.x - dragStartViewport.value.x,
        event.y - dragStartViewport.value.y,
      )
      if (distance > 5) {
        dragMoved.value = true
      }
    }

    const graphPosition = sigma.viewportToGraph({
      x: event.x,
      y: event.y,
    })

    if (!Number.isFinite(graphPosition.x) || !Number.isFinite(graphPosition.y)) {
      return
    }

    graph.setNodeAttribute(draggedNodeId.value, 'x', graphPosition.x)
    graph.setNodeAttribute(draggedNodeId.value, 'y', graphPosition.y)
    event.preventSigmaDefault()
    event.original.preventDefault()
    event.original.stopPropagation()
    safeRefreshPartial({ nodes: [draggedNodeId.value] })
  })

  mouseCaptor.on('mouseup', () => {
    if (!draggedNodeId.value) {
      const pendingNodeId = pendingDragNodeId.value
      if (pendingNodeId) {
        clearPendingNodeDrag(sigma)
        if (graph?.hasNode(pendingNodeId)) {
          ignoreStageClickUntil.value = Date.now() + 180
          skipNextFocusViewportSync.value = true
          emit('selectNode', pendingNodeId)
          return
        }
      }
      return
    }
    if (dragMoved.value) {
      suppressNodeSelectionUntil.value = Date.now() + 260
      ignoreStageClickUntil.value = suppressNodeSelectionUntil.value
    }
    draggedNodeId.value = null
    dragStartViewport.value = null
    dragMoved.value = false
    clearPendingNodeDrag(sigma)
    canvasRef.value?.classList.remove('is-dragging')
  })
}

function mountSigmaGraph(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
): void {
  if (!canvasRef.value) {
    return
  }

  if (!supportsWebGL()) {
    webglUnavailable.value = true
    renderMode.value = 'placeholder'
    emit('rendererState', false)
    emit('ready', {
      fitViewport: () => undefined,
      zoomIn: () => undefined,
      zoomOut: () => undefined,
    })
    return
  }

  let sigma: Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>
  try {
    sigma = createSigma(graph, canvasRef.value)
  } catch {
    renderMode.value = 'placeholder'
    emit('rendererState', false)
    emit('ready', {
      fitViewport: () => undefined,
      zoomIn: () => undefined,
      zoomOut: () => undefined,
    })
    return
  }

  optimizeSigmaRuntime(
    sigma,
    denseOverview.value,
  )
  graphRef.value = graph
  sigmaRef.value = sigma
  webglUnavailable.value = false
  emit('rendererState', true)
  registerWebglContextLossHandler(canvasRef.value)
  registerSigmaInteractions(sigma)

  emit('ready', {
    fitViewport: () => { fitViewport() },
    zoomIn: () => { zoomIn() },
    zoomOut: () => { zoomOut() },
  })

  window.setTimeout(() => {
    if (props.focusedNodeId) {
      fitViewport(0)
    }
    didInitialFit.value = true
  }, 0)
}

function rebuildGraph(): void {
  if (!canvasRef.value) {
    if (webglUnavailable.value) {
      return
    }
    if (renderMode.value !== 'sigma') {
      renderMode.value = 'sigma'
      void nextTick().then(() => {
        rebuildGraph()
      })
    }
    return
  }

  destroyGraph()
  mountSigmaGraph(
    createGraphModel(
      baseNodes.value,
      baseEdges.value,
      props.focusedNodeId,
      props.layoutMode,
      { applyLayout: true },
    ),
  )
}

function buildTargetGraphModel(): MultiUndirectedGraph<
  GraphCanvasNodeAttributes,
  GraphCanvasEdgeAttributes
> {
  return createGraphModel(
    baseNodes.value,
    baseEdges.value,
    props.focusedNodeId,
    props.layoutMode,
    { applyLayout: false },
  )
}

function applyTargetGraph(
  targetGraph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
  options: {
    preserveNodePositions: boolean
  },
): void {
  const graph = graphRef.value
  if (!graph) {
    return
  }

  const currentNodeIds = new Set(graph.nodes())
  const targetNodeIds = new Set(targetGraph.nodes())

  currentNodeIds.forEach((nodeId) => {
    if (!targetNodeIds.has(nodeId)) {
      graph.dropNode(nodeId)
    }
  })

  targetGraph.forEachNode((nodeId, targetAttributes) => {
    if (graph.hasNode(nodeId)) {
      const currentAttributes = graph.getNodeAttributes(nodeId)
      graph.replaceNodeAttributes(nodeId, {
        ...targetAttributes,
        x: options.preserveNodePositions ? currentAttributes.x : targetAttributes.x,
        y: options.preserveNodePositions ? currentAttributes.y : targetAttributes.y,
      })
      return
    }

    graph.addNode(nodeId, targetAttributes)
  })

  graph.clearEdges()
  targetGraph.forEachEdge((_, attributes, source, target) => {
    if (!graph.hasNode(source) || !graph.hasNode(target)) {
      return
    }
    graph.addEdge(source, target, attributes)
  })
}

function graphStructureMatchesSource(
  graph: MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
): boolean {
  if (graph.order !== baseNodes.value.length || graph.size !== baseEdges.value.length) {
    return false
  }

  for (const node of baseNodes.value) {
    if (!graph.hasNode(node.id)) {
      return false
    }
  }

  const expectedEdgeIds = new Set(baseEdges.value.map((edge) => edge.id))
  let valid = true
  graph.forEachEdge((_, attributes) => {
    if (!expectedEdgeIds.has(attributes.edgeId)) {
      valid = false
    }
  })

  return valid
}

function syncGraphData(options?: {
  relayout?: boolean
  fitViewport?: boolean
  styleOnly?: boolean
}): void {
  const graph = graphRef.value
  const sigma = sigmaRef.value
  if (!graph || !sigma) {
    rebuildGraph()
    return
  }

  if (draggedNodeId.value) {
    return
  }

  if (
    options?.styleOnly &&
    !options?.relayout &&
    graph.order === baseNodes.value.length &&
    graph.size === baseEdges.value.length
  ) {
    const refreshDelta = applyGraphVisualState(
      graph,
      baseNodes.value,
      baseEdges.value,
      props.focusedNodeId,
    )
    safeRefreshPartial(
      {
        nodes: refreshDelta.nodeIds,
        edges: refreshDelta.edgeKeys,
      },
    )
    if (options.fitViewport) {
      fitViewport(180)
    }
    return
  }

  if (cancelRelayoutAnimation) {
    cancelRelayoutAnimation()
    cancelRelayoutAnimation = null
  }

  const targetGraph = options?.relayout
    ? createGraphModel(
        baseNodes.value,
        baseEdges.value,
        props.focusedNodeId,
        props.layoutMode,
        { applyLayout: true },
      )
    : buildTargetGraphModel()

  const needsStructureSync = !graphStructureMatchesSource(graph)
  if (!options?.relayout || needsStructureSync) {
    applyTargetGraph(targetGraph, {
      preserveNodePositions: Boolean(options?.relayout),
    })
  }

  if (!options?.relayout) {
    safeRefreshAll()
    if (options?.fitViewport) {
      fitViewport(0)
    }
    return
  }

  const positions = targetGraph.reduceNodes<Record<string, { x: number; y: number }>>(
    (acc, nodeId, attributes) => {
      if (graph.hasNode(nodeId)) {
        acc[nodeId] = { x: attributes.x, y: attributes.y }
      }
      return acc
    },
    {},
  )

  if (Object.keys(positions).length === 0) {
    safeRefreshAll()
    if (options.fitViewport || props.focusedNodeId) {
      fitViewport(0)
    }
    return
  }

  const relayoutDuration = denseOverview.value || denseFocusedGraph.value ? 520 : 420

  if (relayoutTimer !== null) {
    window.clearTimeout(relayoutTimer)
    relayoutTimer = null
  }
  cancelRelayoutAnimation = animateRelayoutNodes(graph, positions, relayoutDuration, sigma)
  relayoutTimer = window.setTimeout(() => {
    relayoutTimer = null
    cancelRelayoutAnimation = null
    safeRefreshAll()
    if (options.fitViewport || props.focusedNodeId) {
      fitViewport(0)
    }
  }, relayoutDuration + 24)
}

watch(
  denseOverview,
  (isDenseOverview) => {
    const sigma = sigmaRef.value
    if (!sigma) {
      return
    }
    optimizeSigmaRuntime(sigma, isDenseOverview)
  },
)

watch(
  () => props.filter,
  async () => {
    await nextTick()
    syncGraphData()
  },
  { immediate: true },
)

watch(
  () => props.showFilteredArtifacts,
  async () => {
    await nextTick()
    syncGraphData()
  },
)

watch(
  () => [props.surfaceVersion, props.nodes.length, props.edges.length] as const,
  async () => {
    await nextTick()
    syncGraphData()
  },
)

watch(
  () => props.layoutMode,
  async () => {
    await nextTick()
    syncGraphData({ relayout: true, fitViewport: Boolean(props.focusedNodeId) })
  },
)

watch(
  () => props.focusedNodeId,
  async () => {
    await nextTick()
    const skipViewportFit = skipNextFocusViewportSync.value
    const fitViewport = Boolean(props.focusedNodeId) && !skipViewportFit
    syncGraphData({ fitViewport, styleOnly: true })
    skipNextFocusViewportSync.value = false
  },
)

onBeforeUnmount(() => {
  destroyGraph()
})
</script>

<template>
  <div class="rr-graph-canvas">
    <div
      v-if="renderMode === 'placeholder'"
      class="rr-graph-canvas__placeholder"
    >
      <strong>{{ t('graph.webglUnavailableTitle') }}</strong>
      <p>{{ t('graph.webglUnavailableDescription') }}</p>
    </div>
    <div
      v-else
      ref="canvasRef"
      class="rr-graph-canvas__stage"
    />
  </div>
</template>
