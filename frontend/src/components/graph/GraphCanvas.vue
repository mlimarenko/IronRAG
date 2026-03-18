<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import Sigma from 'sigma'
import { animateNodes } from 'sigma/utils'
import { NodeBorderProgram } from '@sigma/node-border'
import { createEdgeCurveProgram } from '@sigma/edge-curve'
import type { MultiUndirectedGraph } from 'graphology'
import type { SigmaNodeEventPayload, SigmaStageEventPayload } from 'sigma/types'
import {
  aggregateGraphEdges,
  buildDegreeMap,
  buildNodeMap,
  createGraphModel,
  ensureFinitePositions,
  fallbackPosition,
  filterFocusedNodes,
  type GraphCanvasEdgeAttributes,
  type GraphCanvasNodeAttributes,
} from './graphCanvasModel'
import type { GraphEdge, GraphLayoutMode, GraphNode, GraphNodeType } from 'src/models/ui/graph'

const props = defineProps<{
  nodes: GraphNode[]
  edges: GraphEdge[]
  filter: GraphNodeType | ''
  focusedNodeId: string | null
  focusActive: boolean
  layoutMode: GraphLayoutMode
  surfaceVersion: number
}>()

const emit = defineEmits<{
  selectNode: [id: string]
  clearFocus: []
  ready: [controls: { fitViewport: () => void; zoomIn: () => void; zoomOut: () => void }]
}>()

const { t } = useI18n()

const canvasRef = ref<HTMLDivElement | null>(null)
const sigmaRef = shallowRef<Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes> | null>(null)
const graphRef = shallowRef<
  MultiUndirectedGraph<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes> | null
>(null)
const draggedNodeId = ref<string | null>(null)
const dragStartViewport = ref<{ x: number; y: number } | null>(null)
const dragMoved = ref(false)
const ignoreStageClickUntil = ref(0)
const suppressNodeSelectionUntil = ref(0)
const didInitialFit = ref(false)
const renderMode = ref<'sigma' | 'placeholder'>('sigma')
const webglContextCleanup = ref<(() => void) | null>(null)
const webglUnavailable = ref(false)
const effectiveFocusedNodeId = computed(() =>
  props.focusActive ? props.focusedNodeId : null,
)

const baseNodes = computed(() =>
  props.filter === '' ? props.nodes : props.nodes.filter((node) => node.nodeType === props.filter),
)

const baseAggregatedEdges = computed(() => aggregateGraphEdges(baseNodes.value, props.edges))
const baseDegreeMap = computed(() => buildDegreeMap(baseNodes.value, baseAggregatedEdges.value))
const filteredNodes = computed(() =>
  filterFocusedNodes(
    baseNodes.value,
    baseAggregatedEdges.value,
    effectiveFocusedNodeId.value,
    baseDegreeMap.value,
  ),
)
const visibleNodeMap = computed(() => buildNodeMap(filteredNodes.value))
const aggregatedEdges = computed(() =>
  baseAggregatedEdges.value.filter(
    (edge) => visibleNodeMap.value.has(edge.source) && visibleNodeMap.value.has(edge.target),
  ),
)

function supportsWebGL(): boolean {
  const canvas = document.createElement('canvas')
  return Boolean(canvas.getContext('webgl2') ?? canvas.getContext('webgl'))
}

function fitViewport(duration = 260): void {
  const sigma = sigmaRef.value
  if (!sigma) {
    return
  }

  sigma.setCustomBBox(null)
  void sigma.getCamera().animatedReset({ duration })
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
  const denseOverview = !effectiveFocusedNodeId.value && filteredNodes.value.length > 120
  const settings = {
    allowInvalidContainer: true,
    defaultNodeType: 'default',
    defaultEdgeType: 'curvedNoArrow',
    renderLabels: true,
    renderEdgeLabels: false,
    hideEdgesOnMove: false,
    hideLabelsOnMove: true,
    enableEdgeEvents: false,
    labelDensity: denseOverview ? 0.03 : 0.22,
    labelGridCellSize: denseOverview ? 160 : 92,
    labelRenderedSizeThreshold: denseOverview ? 18.4 : 12.9,
    labelSize: 12,
    minCameraRatio: 0.05,
    maxCameraRatio: 4,
    autoRescale: true,
    autoCenter: true,
    nodeProgramClasses: {
      default: NodeBorderProgram as never,
    },
    edgeProgramClasses: {
      curvedNoArrow: createEdgeCurveProgram(),
    },
  } satisfies ConstructorParameters<
    typeof Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>
  >[2]

  try {
    return new Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>(
      graph,
      container,
      settings,
    )
  } catch (error) {
    if (recoverInvalidNodePosition(error, graph)) {
      return new Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>(
        graph,
        container,
        settings,
      )
    }
    throw error
  }
}

function safeRefreshPartial(nodeIds: string[]): void {
  const sigma = sigmaRef.value
  if (!sigma) {
    return
  }

  if (graphRef.value) {
    ensureFinitePositions(graphRef.value)
  }

  try {
    sigma.refresh({ partialGraph: { nodes: nodeIds }, skipIndexation: false })
  } catch (error) {
    if (recoverInvalidNodePosition(error)) {
      sigma.refresh({ partialGraph: { nodes: nodeIds }, skipIndexation: false })
      return
    }
    throw error
  }
}

function safeRefreshAll(): void {
  const sigma = sigmaRef.value
  if (!sigma) {
    return
  }

  if (graphRef.value) {
    ensureFinitePositions(graphRef.value)
  }

  try {
    sigma.refresh()
  } catch (error) {
    if (recoverInvalidNodePosition(error)) {
      sigma.refresh()
      return
    }
    throw error
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
  if (webglContextCleanup.value) {
    webglContextCleanup.value()
    webglContextCleanup.value = null
  }
  if (sigmaRef.value) {
    sigmaRef.value.kill()
    sigmaRef.value = null
  }
  graphRef.value = null
  draggedNodeId.value = null
  dragStartViewport.value = null
  dragMoved.value = false
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
  }

  canvas.addEventListener('webglcontextlost', handleContextLost, false)
  webglContextCleanup.value = () => {
    canvas.removeEventListener('webglcontextlost', handleContextLost, false)
  }
}

function registerSigmaInteractions(
  sigma: Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>,
): void {
  const shouldStartNodeDrag = (event: SigmaNodeEventPayload['event']): boolean =>
    event.original.shiftKey

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
      const hitRadius = Math.max(14, attributes.size * 1.7)

      if (distance <= hitRadius && distance < nearestDistance) {
        nearestDistance = distance
        nearestNodeId = nodeId
      }
    })

    return nearestNodeId
  }

  sigma.on('clickNode', ({ node }: SigmaNodeEventPayload) => {
    if (Date.now() < suppressNodeSelectionUntil.value || dragMoved.value) {
      return
    }
    ignoreStageClickUntil.value = Date.now() + 180
    emit('selectNode', node)
  })

  sigma.on('enterNode', () => {
    canvasRef.value?.classList.add('is-hovering-node')
  })

  sigma.on('leaveNode', () => {
    canvasRef.value?.classList.remove('is-hovering-node')
  })

  sigma.on('clickStage', ({ event }: SigmaStageEventPayload) => {
    if (
      Date.now() < ignoreStageClickUntil.value ||
      Date.now() < suppressNodeSelectionUntil.value
    ) {
      return
    }

    const fallbackNodeId = findNearestNodeAtViewportPoint(event.x, event.y)
    if (fallbackNodeId) {
      emit('selectNode', fallbackNodeId)
      return
    }

    emit('clearFocus')
  })

  sigma.on('downNode', ({ node, event }: SigmaNodeEventPayload) => {
    if (!shouldStartNodeDrag(event)) {
      return
    }
    draggedNodeId.value = node
    dragStartViewport.value = { x: event.x, y: event.y }
    dragMoved.value = false
    sigma.setSetting('enableCameraPanning', false)
    sigma.setCustomBBox(sigma.getBBox())
    canvasRef.value?.classList.add('is-dragging')
  })

  const graph = graphRef.value
  const mouseCaptor = sigma.getMouseCaptor()

  const handleMouseMoveBody = (event: SigmaStageEventPayload['event']) => {
    if (!draggedNodeId.value || !graph?.hasNode(draggedNodeId.value)) {
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
    safeRefreshPartial([draggedNodeId.value])
  }

  const handleMouseUp = () => {
    if (!draggedNodeId.value) {
      return
    }
    if (dragMoved.value) {
      suppressNodeSelectionUntil.value = Date.now() + 260
      ignoreStageClickUntil.value = suppressNodeSelectionUntil.value
    }
    draggedNodeId.value = null
    dragStartViewport.value = null
    dragMoved.value = false
    sigma.setSetting('enableCameraPanning', true)
    canvasRef.value?.classList.remove('is-dragging')
  }

  mouseCaptor.on('mousemovebody', handleMouseMoveBody)
  mouseCaptor.on('mouseup', handleMouseUp)
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
    emit('ready', {
      fitViewport: () => {
        return undefined
      },
      zoomIn: () => {
        return undefined
      },
      zoomOut: () => {
        return undefined
      },
    })
    return
  }

  let sigma: Sigma<GraphCanvasNodeAttributes, GraphCanvasEdgeAttributes>
  try {
    sigma = createSigma(graph, canvasRef.value)
  } catch {
    renderMode.value = 'placeholder'
    emit('ready', {
      fitViewport: () => {
        return undefined
      },
      zoomIn: () => {
        return undefined
      },
      zoomOut: () => {
        return undefined
      },
    })
    return
  }

  graphRef.value = graph
  sigmaRef.value = sigma
  webglUnavailable.value = false
  registerWebglContextLossHandler(canvasRef.value)
  registerSigmaInteractions(sigma)

  emit('ready', {
    fitViewport: () => {
      fitViewport()
    },
    zoomIn: () => {
      zoomIn()
    },
    zoomOut: () => {
      zoomOut()
    },
  })

  window.setTimeout(() => {
    if (!didInitialFit.value) {
      fitViewport(0)
      didInitialFit.value = true
    }
    if (effectiveFocusedNodeId.value) {
      fitViewport(0)
    }
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
      filteredNodes.value,
      aggregatedEdges.value,
      effectiveFocusedNodeId.value,
      props.layoutMode,
    ),
  )
}

function syncGraphData(): void {
  const graph = graphRef.value
  const sigma = sigmaRef.value
  if (!graph || !sigma) {
    rebuildGraph()
    return
  }

  if (draggedNodeId.value) {
    return
  }

  const targetGraph = createGraphModel(
    filteredNodes.value,
    aggregatedEdges.value,
    effectiveFocusedNodeId.value,
    props.layoutMode,
  )

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
        x: currentAttributes.x,
        y: currentAttributes.y,
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

  safeRefreshAll()
}

function relayoutGraph(): void {
  const graph = graphRef.value
  const sigma = sigmaRef.value
  if (!graph || !sigma) {
    rebuildGraph()
    return
  }

  const targetGraph = createGraphModel(
    filteredNodes.value,
    aggregatedEdges.value,
    effectiveFocusedNodeId.value,
    props.layoutMode,
  )
  const positions = targetGraph.reduceNodes<Record<string, { x: number; y: number }>>(
    (acc, nodeId, attributes) => {
      if (graph.hasNode(nodeId)) {
        acc[nodeId] = { x: attributes.x, y: attributes.y }
      }
      return acc
    },
    {},
  )

  animateNodes(graph, positions, { duration: 260 })
  window.setTimeout(() => {
    safeRefreshAll()
    if (effectiveFocusedNodeId.value) {
      fitViewport(0)
    }
  }, 280)
}

watch(
  () => props.filter,
  async () => {
    await nextTick()
    syncGraphData()
  },
  { immediate: true },
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
    relayoutGraph()
  },
)

watch(
  () => props.focusActive,
  async () => {
    await nextTick()
    rebuildGraph()
    if (props.focusActive) {
      window.setTimeout(() => {
        fitViewport(0)
      }, 0)
    }
  },
)

watch(
  () => props.focusedNodeId,
  async () => {
    await nextTick()
    if (props.focusActive) {
      rebuildGraph()
      window.setTimeout(() => {
        fitViewport(0)
      }, 0)
    }
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
