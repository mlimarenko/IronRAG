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
const ignoreStageClickUntil = ref(0)
const renderMode = ref<'sigma' | 'placeholder'>('sigma')

const baseNodes = computed(() =>
  props.filter === '' ? props.nodes : props.nodes.filter((node) => node.nodeType === props.filter),
)

const baseAggregatedEdges = computed(() => aggregateGraphEdges(baseNodes.value, props.edges))
const baseDegreeMap = computed(() => buildDegreeMap(baseNodes.value, baseAggregatedEdges.value))
const filteredNodes = computed(() =>
  filterFocusedNodes(
    baseNodes.value,
    baseAggregatedEdges.value,
    props.focusedNodeId,
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
  const denseOverview = !props.focusedNodeId && filteredNodes.value.length > 120
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

function focusNode(nodeId: string | null): void {
  const sigma = sigmaRef.value
  const graph = graphRef.value
  if (!sigma || !graph || !nodeId || !graph.hasNode(nodeId)) {
    fitViewport()
    return
  }

  const { x, y } = graph.getNodeAttributes(nodeId)
  const camera = sigma.getCamera()
  const nextRatio = Math.max(0.32, Math.min(camera.ratio * 0.84, 0.76))
  void camera.animate({ x, y, ratio: nextRatio, angle: 0 }, { duration: 220 })
}

function destroyGraph(): void {
  if (sigmaRef.value) {
    sigmaRef.value.kill()
    sigmaRef.value = null
  }
  graphRef.value = null
  draggedNodeId.value = null
  renderMode.value = 'sigma'
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
      const hitRadius = Math.max(14, attributes.size * 1.7)

      if (distance <= hitRadius && distance < nearestDistance) {
        nearestDistance = distance
        nearestNodeId = nodeId
      }
    })

    return nearestNodeId
  }

  sigma.on('clickNode', ({ node }: SigmaNodeEventPayload) => {
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
    if (Date.now() < ignoreStageClickUntil.value) {
      return
    }

    const fallbackNodeId = findNearestNodeAtViewportPoint(event.x, event.y)
    if (fallbackNodeId) {
      emit('selectNode', fallbackNodeId)
      return
    }

    emit('clearFocus')
  })

  sigma.on('downNode', ({ node }: SigmaNodeEventPayload) => {
    draggedNodeId.value = node
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
    draggedNodeId.value = null
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
    fitViewport(0)
    if (props.focusedNodeId) {
      focusNode(props.focusedNodeId)
    }
  }, 0)
}

function rebuildGraph(): void {
  if (!canvasRef.value) {
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
      props.focusedNodeId,
      props.layoutMode,
    ),
  )
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
    props.focusedNodeId,
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
    if (props.focusedNodeId) {
      focusNode(props.focusedNodeId)
    }
  }, 280)
}

watch(
  () =>
    [
      props.surfaceVersion,
      props.filter,
      props.focusedNodeId,
      props.nodes.length,
      props.edges.length,
    ] as const,
  async () => {
    await nextTick()
    rebuildGraph()
  },
  { immediate: true },
)

watch(
  () => props.layoutMode,
  async () => {
    await nextTick()
    relayoutGraph()
  },
)

watch(
  () => props.focusedNodeId,
  async (nodeId) => {
    await nextTick()
    focusNode(nodeId)
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
