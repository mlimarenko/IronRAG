import {
  memo,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react'
import { useTranslation } from 'react-i18next'
import Graph from 'graphology'
import Sigma from 'sigma'
import type { CameraState } from 'sigma/types'
import { Loader2 } from 'lucide-react'
import { EdgeCurvedArrowProgram } from '@sigma/edge-curve'
import type { GraphNode } from '@/shared/types'
import {
  buildGraphCanvasLabel,
  buildGraphFocusLabel,
  GRAPH_EDGE_DENSE_RENDER_CAP,
  GRAPH_EDGE_COLORS,
  GRAPH_EDGE_RENDER_CAP,
  GRAPH_NODE_COLORS,
  isIterativeLayout,
  selectProminentGraphLabelIds,
  type GraphLayoutType,
} from '@/features/graph/model/config'
import { applyGraphLayout } from '@/features/graph/model/layouts'
import { buildNodeSizer, emphasizeNodeSize } from '@/features/graph/model/graphSizing'
import { computeGraphLayoutOffThread } from '@/features/graph/workers/graphLayoutClient'
import {
  createAllEdgesLayerState,
  createCoordinateAllEdgesLayerState,
  type AllEdgesIndexedLayerState,
  type AllEdgesLayerState,
} from './allEdgesLayerProgram'
import { PreferencesContext } from '@/shared/contexts/preferences-context'
import {
  applyLayoutPositionsChunked,
  buildTooltipData,
  createChunkedEdgeRestorer,
  refreshEdgeChunks,
  updatePositionTextureFromLayoutChunked,
} from './sigmaGraphRuntime'

interface EdgeData {
  id: string
  sourceId: string
  targetId: string
  label: string
  weight: number
}

interface SigmaGraphProps {
  /** Full topology, not a filtered projection. Re-building the Graphology
   *  instance on every keystroke is a catastrophic cost on 100k-node graphs
   *  (seconds of layout + re-init per key), so filters are applied via
   *  Sigma's reducer pipeline instead of by rebuilding the graph. */
  nodes: GraphNode[]
  edges: EdgeData[]
  selectedId: string | null
  onSelect: (id: string | null) => void
  layout: GraphLayoutType
  /** Canonical "hide this node" set. Empty means everything visible.
   *  Owned by the parent so search / legend toggles can drive the filter
   *  without touching the Graphology instance. */
  hiddenIds?: Set<string>
  /** Called once after Sigma is initialized with a stable `fitView`
   *  callback. The parent stores this and calls it from the toolbar. */
  onFitViewReady?: (fitView: () => void) => void
  /** When false (default), draw the smooth overview edge sample. When true,
   *  draw a denser Sigma base sample while keeping a hard render cap. The
   *  full topology still drives adjacency, selection, inspectors, and the
   *  dense GPU overlay; Sigma itself only carries the interaction-friendly
   *  sample so Firefox does not reindex hundreds of thousands of edges. */
  showDenseEdges?: boolean
}

type SigmaPointerCaptorEvent = {
  x: number
  y: number
  preventSigmaDefault: () => void
  // Sigma's `MouseCoords.original` is typed `MouseEvent | TouchEvent` even
  // though `getMouseCoords()` (the only producer of `mousemovebody` events)
  // always constructs it from a `MouseEvent`. Match the wider upstream type
  // here so the local handler stays assignable to sigma's `on()` signature;
  // callers narrow with `instanceof MouseEvent` where they need
  // mouse-only members (e.g. `clientX`/`clientY`).
  original: MouseEvent | TouchEvent
}

type SigmaReducerData = {
  size?: number
  label?: string
  displayLabel?: string
  focusLabel?: string
  highlighted?: boolean
  [key: string]: unknown
}

type NeighborhoodOverlayMode = 'hover' | 'selected' | 'drag'

type NeighborhoodOverlayFocus = {
  nodeId: string
  mode: NeighborhoodOverlayMode
} | null

const LAYOUT_ANIMATION_DURATION_MS = 280
/// Stable empty-set sentinel for hidden-edge lookups. Using one shared
/// reference avoids allocating a throwaway `new Set()` inside the hot
/// reducer effect on every run.
const EMPTY_EDGE_SET: ReadonlySet<string> = new Set()
/// Matching empty-set sentinel for the prominent-label lookup. Skipping
/// the O(N log N) sort inside `selectProminentGraphLabelIds` at
/// ultra-dense node counts means we short-circuit to this shared set
/// instead of allocating an empty one per rebuild.
const EMPTY_LABEL_SET: ReadonlySet<string> = new Set()
const SIGMA_NODE_CANVAS_LAYERS = ['nodes', 'labels', 'hovers', 'hoverNodes', 'mouse'] as const
const waitForAnimationFrame = () =>
  new Promise<void>((resolve) => {
    requestAnimationFrame(() => resolve())
  })

function denseEdgeStyle(isDense: boolean, theme: string | undefined) {
  if (!isDense) return { color: GRAPH_EDGE_COLORS.regular, size: 0.42 }
  return {
    color: theme === 'dark' ? GRAPH_EDGE_COLORS.dense : GRAPH_EDGE_COLORS.denseLight,
    size: theme === 'light' ? 0.34 : 0.28,
  }
}

function labelDensityForNodeCount(nodeCount: number): number {
  if (nodeCount > 900) return 0.016
  if (nodeCount > 450) return 0.022
  return 0.045
}

function labelRenderedThreshold(nodeCount: number, isDisabled: boolean): number {
  if (isDisabled) return 9999
  if (nodeCount > 5000) return 14
  if (nodeCount > 900) return 10
  return 8
}

function lodThresholdForRatio(
  ratio: number,
  tiers: Readonly<{ overview: number; mid: number; near: number }>,
): number {
  if (ratio >= 0.6) return tiers.overview
  if (ratio >= 0.3) return tiers.mid
  if (ratio >= 0.12) return tiers.near
  return 0
}

function overlayColors(mode: NeighborhoodOverlayMode) {
  if (mode === 'drag') {
    return { stroke: 'rgba(251, 191, 36, 0.84)', halo: 'rgba(251, 191, 36, 0.28)' }
  }
  if (mode === 'selected') {
    return { stroke: 'rgba(245, 158, 11, 0.82)', halo: 'rgba(245, 158, 11, 0.24)' }
  }
  return { stroke: 'rgba(226, 232, 240, 0.48)', halo: 'rgba(226, 232, 240, 0.14)' }
}

function createLodScheduler({
  visibleNodeCount,
  sigma,
  getThreshold,
  setThreshold,
  tiers,
  isCurrent,
}: {
  visibleNodeCount: number
  sigma: Sigma
  getThreshold: () => number
  setThreshold: (threshold: number) => void
  tiers: Readonly<{ overview: number; mid: number; near: number }>
  isCurrent: () => boolean
}) {
  const apply = () => {
    if (visibleNodeCount <= LOD_NODE_THRESHOLD) {
      if (getThreshold() !== 0) setThreshold(0)
      return
    }
    const next = lodThresholdForRatio(sigma.getCamera().ratio, tiers)
    if (next !== getThreshold()) setThreshold(next)
  }

  let timer: ReturnType<typeof setTimeout> | null = null
  const schedule = () => {
    if (visibleNodeCount <= LOD_NODE_THRESHOLD) return
    if (timer != null) clearTimeout(timer)
    timer = setTimeout(() => {
      timer = null
      if (!isCurrent()) return
      apply()
      sigma.refresh({})
    }, 220)
  }

  return { apply, schedule, clear: () => timer != null && clearTimeout(timer) }
}
/// Above this node count, layout transitions are applied instantly
/// (no per-frame interpolation). At 5000+ nodes the animation burns
/// 1.5M setNodeAttribute calls per second and provides no visual
/// value — the human eye cannot track thousands of dots drifting at
/// once. Matches the density tier used for label throttling above.
const INSTANT_LAYOUT_NODE_THRESHOLD = 5000
/// Above this node count, labels are disabled entirely. Sigma's label
/// collision detection is the dominant cost per frame even with
/// `hideLabelsOnMove` and `labelRenderedSizeThreshold` tuned up; at
/// 15k+ nodes the labels are visually useless anyway (unreadable at
/// that density) and turning them off shaves meaningful work from the
/// dense-graph per-frame budget.
const LABELS_DISABLED_NODE_THRESHOLD = 15000
/// Above this node count, the initial layout is computed in a Web
/// Worker so it never blocks the main thread. Below it, the sync
/// codepath is cheaper: serializing the node/edge arrays, spinning up
/// a postMessage round-trip, and deserializing the float positions is
/// ~20 ms of overhead that is not recovered on tiny graphs. 3000 is
/// roughly where `applyGraphLayout` starts to exceed a 16 ms frame
/// budget, so the crossover lines up naturally.
const GRAPH_WORKER_NODE_THRESHOLD = 3000
/// Above this node count, pointer interactions must not repaint graph-wide
/// neighborhoods through Sigma. Dense graphs use DOM/canvas affordances for
/// hover and incident drag edges; the only live Sigma repaint allowed during
/// drag is the single node under the cursor.
const DOM_ONLY_INTERACTION_NODE_THRESHOLD = 15000
/// Local edge affordance cap for the DOM/canvas overlay. The full topology
/// remains available to the inspector, but drawing tens of thousands of
/// incident lines for one hub on every drag/camera frame would move the same
/// Firefox bottleneck into 2D canvas.
const NEIGHBORHOOD_OVERLAY_EDGE_LIMIT = 1200
const DRAG_NEIGHBORHOOD_OVERLAY_EDGE_LIMIT = 320
const BASELINE_EDGE_ENDPOINT_COVERAGE_RATIO = 0.6
const BASELINE_EDGE_LOCAL_DETAIL_RATIO = 0.9
const ALL_EDGES_LAYER_NODE_THRESHOLD = 15000
const ALL_EDGES_LAYER_EDGE_THRESHOLD = GRAPH_EDGE_RENDER_CAP
/// Above this visible-node count, zoom+degree LOD engages: the overview shows a
/// hub backbone and zooming in reveals the rest. Below it, every node always
/// renders (small/mid graphs are legible whole).
const LOD_NODE_THRESHOLD = 15000
/// Target visible-node budgets per LOD tier (overview → mid → near zoom). The
/// degree threshold for each is derived from the actual degree distribution so
/// the backbone size is stable regardless of the graph's shape.
const LOD_TIER_BUDGETS = { overview: 2500, mid: 9000, near: 30000 } as const
const ALL_EDGES_LAYER_DARK_COLOR: readonly [number, number, number, number] = [
  0.78, 0.84, 0.92, 0.22,
]
const ALL_EDGES_LAYER_LIGHT_COLOR: readonly [number, number, number, number] = [
  0.06, 0.1, 0.18, 0.34,
]
const EDGE_PAIR_KEY_SEPARATOR = '\u001f'

function edgePairKey(sourceId: string, targetId: string): string {
  return `${sourceId}${EDGE_PAIR_KEY_SEPARATOR}${targetId}`
}

function hashSamplePart(hash: number, value: string): number {
  let next = hash
  for (let i = 0; i < value.length; i += 1) {
    next ^= value.codePointAt(i) ?? 0
    next = Math.imul(next, 16777619)
  }
  return next >>> 0
}

function edgeSampleHash(edge: EdgeData): number {
  let hash = 2166136261
  hash = hashSamplePart(hash, edge.id)
  hash = hashSamplePart(hash, edge.sourceId)
  hash = hashSamplePart(hash, edge.targetId)
  return hash >>> 0
}

function baselineEdgeSampleQuota(degree: number): number {
  if (degree >= 1024) return 40
  if (degree >= 512) return 32
  if (degree >= 256) return 24
  if (degree >= 128) return 18
  if (degree >= 64) return 14
  if (degree >= 16) return 10
  if (degree >= 4) return 5
  return 2
}

function applyCheapFallbackLayout(graph: Graph): void {
  const order = graph.order
  if (order === 0) return

  const columns = Math.max(1, Math.ceil(Math.sqrt(order)))
  const rows = Math.max(1, Math.ceil(order / columns))
  const gap = Math.max(8, Math.sqrt(order) * 0.8)
  let index = 0

  graph.updateEachNodeAttributes(
    (_id, attr) => {
      const row = Math.floor(index / columns)
      const column = index % columns
      attr.x = (column - (columns - 1) / 2) * gap
      attr.y = (row - (rows - 1) / 2) * gap
      index += 1
      return attr
    },
    { attributes: ['x', 'y'] },
  )
}

function getCanvas2dContext(canvas: HTMLCanvasElement): CanvasRenderingContext2D | null {
  // JSDOM exposes getContext but only reports "not implemented" through its
  // virtual console. The overlay is visual-only, so unit tests can skip it.
  if (typeof window !== 'undefined' && window.navigator.userAgent.toLowerCase().includes('jsdom')) {
    return null
  }
  return canvas.getContext('2d')
}

// --- Component ---

function populateIndexedNodePositions(
  graph: Graph,
  hidden: ReadonlySet<string> | null | undefined,
): Map<string, number> {
  const nodeIndexById = new Map<string, number>()
  graph.forEachNode((node, attr) => {
    if (hidden?.has(node)) return
    const x = attr.x as number | undefined
    const y = attr.y as number | undefined
    if (x == null || y == null) return
    nodeIndexById.set(node, nodeIndexById.size)
  })
  return nodeIndexById
}

function buildIndexedEdgeData(
  edges: EdgeData[],
  nodeIndexById: ReadonlyMap<string, number>,
  hidden: ReadonlySet<string> | null | undefined,
): { data: Float32Array; edgeCount: number; length: number } {
  const data = new Float32Array(edges.length * 6)
  let length = 0
  let edgeCount = 0
  for (const edge of edges) {
    if (edge.sourceId === edge.targetId) continue
    if (hidden?.has(edge.sourceId) || hidden?.has(edge.targetId)) continue
    const sourceIndex = nodeIndexById.get(edge.sourceId)
    const targetIndex = nodeIndexById.get(edge.targetId)
    if (sourceIndex == null || targetIndex == null) continue
    data[length] = sourceIndex
    data[length + 1] = targetIndex
    data[length + 2] = 0
    data[length + 3] = sourceIndex
    data[length + 4] = targetIndex
    data[length + 5] = 1
    length += 6
    edgeCount += 1
  }
  return { data, edgeCount, length }
}

function buildCoordinateEdgeVertices(
  graph: Graph,
  edges: EdgeData[],
  hidden: ReadonlySet<string> | null | undefined,
): { data: Float32Array; edgeCount: number; length: number } {
  const data = new Float32Array(edges.length * 4)
  let length = 0
  let edgeCount = 0
  for (const edge of edges) {
    if (edge.sourceId === edge.targetId) continue
    if (hidden?.has(edge.sourceId) || hidden?.has(edge.targetId)) continue
    if (!graph.hasNode(edge.sourceId) || !graph.hasNode(edge.targetId)) continue
    const sourceX = graph.getNodeAttribute(edge.sourceId, 'x') as number | undefined
    const sourceY = graph.getNodeAttribute(edge.sourceId, 'y') as number | undefined
    const targetX = graph.getNodeAttribute(edge.targetId, 'x') as number | undefined
    const targetY = graph.getNodeAttribute(edge.targetId, 'y') as number | undefined
    if (sourceX == null || sourceY == null || targetX == null || targetY == null) continue
    data[length] = sourceX
    data[length + 1] = sourceY
    data[length + 2] = targetX
    data[length + 3] = targetY
    length += 4
    edgeCount += 1
  }
  return { data, edgeCount, length }
}

function uploadCoordinateAllEdgesLayer(
  state: Extract<AllEdgesLayerState, { kind: 'coordinate' }>,
  graph: Graph,
  visibleEdges: EdgeData[],
  hidden: ReadonlySet<string> | null | undefined,
  canvas: HTMLCanvasElement,
  scheduleDraw: () => void,
): number {
  const {
    data: vertices,
    edgeCount,
    length,
  } = buildCoordinateEdgeVertices(graph, visibleEdges, hidden)
  const gl = state.gl
  gl.bindBuffer(gl.ARRAY_BUFFER, state.buffer)
  gl.bufferData(
    gl.ARRAY_BUFFER,
    length === vertices.length ? vertices : vertices.subarray(0, length),
    gl.STATIC_DRAW,
  )
  canvas.dataset.allEdgesCount = String(edgeCount)
  canvas.dataset.allEdgesLayer = 'coordinate'
  if (edgeCount > 0) scheduleDraw()
  else {
    gl.viewport(0, 0, canvas.width, canvas.height)
    gl.clearColor(0, 0, 0, 0)
    gl.clear(gl.COLOR_BUFFER_BIT)
  }
  return edgeCount
}

function focusedNodeData(
  node: string,
  data: SigmaReducerData,
  focusId: string,
  neighbors: ReadonlySet<string>,
  hiddenNodes: ReadonlySet<string> | null,
  mode: 'selected' | 'hovered',
): SigmaReducerData {
  if (hiddenNodes?.has(node)) return { ...data, hidden: true, label: '' }
  const isFocus = node === focusId
  if (!isFocus && !neighbors.has(node)) return data
  const label = data.focusLabel ?? data.displayLabel ?? data.label
  let sizeMode: 'selected' | 'hovered' | 'neighbor' | 'hoverNeighbor'
  if (isFocus) sizeMode = mode
  else if (mode === 'selected') sizeMode = 'neighbor'
  else sizeMode = 'hoverNeighbor'
  return {
    ...data,
    size: emphasizeNodeSize(data.size ?? 0, sizeMode),
    ...(label !== undefined ? { label } : {}),
    forceLabel: true,
    ...(isFocus || mode === 'selected' ? { highlighted: true } : {}),
  }
}

function collectValidIds(
  previous: ReadonlySet<string>,
  next: ReadonlySet<string>,
  exists: (id: string) => boolean,
): string[] {
  const result: string[] = []
  const seen = new Set<string>()
  for (const id of previous) {
    if (!seen.has(id) && exists(id)) {
      seen.add(id)
      result.push(id)
    }
  }
  for (const id of next) {
    if (!seen.has(id) && exists(id)) {
      seen.add(id)
      result.push(id)
    }
  }
  return result
}

function createHiddenEdgeReducer(isEdgeHidden: (edge: string) => boolean) {
  return (edge: string, data: SigmaReducerData): SigmaReducerData =>
    isEdgeHidden(edge) ? { ...data, hidden: true } : data
}

function configureSelectedReducers({
  sigma,
  graph,
  selectedId,
  neighborIndex,
  hiddenNodeSet,
  isEdgeHidden,
  nextAffectedNodes,
  nextAffectedEdges,
}: {
  sigma: Sigma
  graph: Graph
  selectedId: string
  neighborIndex: ReadonlyMap<string, Set<string>>
  hiddenNodeSet: ReadonlySet<string> | null
  isEdgeHidden: (edge: string) => boolean
  nextAffectedNodes: Set<string>
  nextAffectedEdges: Set<string>
}): void {
  const connectedEdges = new Set<string>(graph.edges(selectedId))
  const neighbors = neighborIndex.get(selectedId) ?? new Set<string>()
  nextAffectedNodes.add(selectedId)
  for (const neighbor of neighbors) nextAffectedNodes.add(neighbor)
  for (const edge of connectedEdges) nextAffectedEdges.add(edge)
  sigma.setSetting('nodeReducer', (node: string, data: SigmaReducerData) =>
    focusedNodeData(node, data, selectedId, neighbors, hiddenNodeSet, 'selected'),
  )
  sigma.setSetting('edgeReducer', (edge: string, data: SigmaReducerData) => {
    if (isEdgeHidden(edge)) return { ...data, hidden: true }
    if (!connectedEdges.has(edge)) return data
    return { ...data, color: GRAPH_EDGE_COLORS.highlight, size: Math.max(data.size ?? 0, 1.2) }
  })
}

function configureHoveredReducers({
  sigma,
  hoveredId,
  neighborIndex,
  hiddenNodeSet,
  hasHiddenEdges,
  isEdgeHidden,
  nextAffectedNodes,
}: {
  sigma: Sigma
  hoveredId: string
  neighborIndex: ReadonlyMap<string, Set<string>>
  hiddenNodeSet: ReadonlySet<string> | null
  hasHiddenEdges: boolean
  isEdgeHidden: (edge: string) => boolean
  nextAffectedNodes: Set<string>
}): void {
  const neighbors = neighborIndex.get(hoveredId) ?? new Set<string>()
  nextAffectedNodes.add(hoveredId)
  for (const neighbor of neighbors) nextAffectedNodes.add(neighbor)
  sigma.setSetting('nodeReducer', (node: string, data: SigmaReducerData) =>
    focusedNodeData(node, data, hoveredId, neighbors, hiddenNodeSet, 'hovered'),
  )
  sigma.setSetting('edgeReducer', hasHiddenEdges ? createHiddenEdgeReducer(isEdgeHidden) : null)
}

function configureFilterReducers({
  sigma,
  hiddenNodeSet,
  filterHiddenEdgeIds,
  lodHiddenEdgeIds,
  isEdgeHidden,
  nextAffectedNodes,
  nextAffectedEdges,
}: {
  sigma: Sigma
  hiddenNodeSet: ReadonlySet<string> | null
  filterHiddenEdgeIds: ReadonlySet<string>
  lodHiddenEdgeIds: ReadonlySet<string>
  isEdgeHidden: (edge: string) => boolean
  nextAffectedNodes: Set<string>
  nextAffectedEdges: Set<string>
}): void {
  if (hiddenNodeSet) for (const node of hiddenNodeSet) nextAffectedNodes.add(node)
  for (const edge of filterHiddenEdgeIds) nextAffectedEdges.add(edge)
  for (const edge of lodHiddenEdgeIds) nextAffectedEdges.add(edge)
  const nodeReducer = hiddenNodeSet
    ? (node: string, data: SigmaReducerData) =>
        hiddenNodeSet.has(node) ? { ...data, hidden: true, label: '' } : data
    : null
  sigma.setSetting('nodeReducer', nodeReducer)
  sigma.setSetting('edgeReducer', createHiddenEdgeReducer(isEdgeHidden))
}

function clearGraphReducers(sigma: Sigma): void {
  sigma.setSetting('nodeReducer', null)
  sigma.setSetting('edgeReducer', null)
}

function configureInteractionReducers({
  sigma,
  graph,
  selectedId,
  hoveredId,
  neighborIndex,
  hiddenNodeSet,
  filterHiddenEdgeIds,
  lodHiddenEdgeIds,
  hasHiddenEdges,
  isEdgeHidden,
  useDomOnlyInteractions,
  nextAffectedNodes,
  nextAffectedEdges,
}: {
  sigma: Sigma
  graph: Graph
  selectedId: string | null
  hoveredId: string | null
  neighborIndex: ReadonlyMap<string, Set<string>>
  hiddenNodeSet: ReadonlySet<string> | null
  filterHiddenEdgeIds: ReadonlySet<string>
  lodHiddenEdgeIds: ReadonlySet<string>
  hasHiddenEdges: boolean
  isEdgeHidden: (edge: string) => boolean
  useDomOnlyInteractions: boolean
  nextAffectedNodes: Set<string>
  nextAffectedEdges: Set<string>
}): 'dom-idle' | 'configured' {
  if (!hiddenNodeSet && !hasHiddenEdges && useDomOnlyInteractions) {
    clearGraphReducers(sigma)
    return 'dom-idle'
  }
  if (!useDomOnlyInteractions && selectedId && graph.hasNode(selectedId)) {
    configureSelectedReducers({
      sigma,
      graph,
      selectedId,
      neighborIndex,
      hiddenNodeSet,
      isEdgeHidden,
      nextAffectedNodes,
      nextAffectedEdges,
    })
    return 'configured'
  }
  if (!useDomOnlyInteractions && hoveredId && graph.hasNode(hoveredId)) {
    configureHoveredReducers({
      sigma,
      hoveredId,
      neighborIndex,
      hiddenNodeSet,
      hasHiddenEdges,
      isEdgeHidden,
      nextAffectedNodes,
    })
    return 'configured'
  }
  if (hiddenNodeSet || hasHiddenEdges) {
    configureFilterReducers({
      sigma,
      hiddenNodeSet,
      filterHiddenEdgeIds,
      lodHiddenEdgeIds,
      isEdgeHidden,
      nextAffectedNodes,
      nextAffectedEdges,
    })
    return 'configured'
  }
  clearGraphReducers(sigma)
  return 'configured'
}

function uploadIndexedAllEdgesLayer({
  state,
  graph,
  visibleEdges,
  hidden,
  canvas,
  onState,
  onCounts,
  scheduleDraw,
  clearLayer,
}: {
  state: AllEdgesIndexedLayerState
  graph: Graph
  visibleEdges: EdgeData[]
  hidden: ReadonlySet<string> | null | undefined
  canvas: HTMLCanvasElement
  onState: (state: AllEdgesLayerState | null) => void
  onCounts: (edgeCount: number) => void
  scheduleDraw: () => void
  clearLayer: () => void
}): AllEdgesLayerState | null {
  const gl = state.gl
  const nodeIndexById = populateIndexedNodePositions(graph, hidden)
  if (nodeIndexById.size === 0) {
    onCounts(0)
    gl.clearColor(0, 0, 0, 0)
    gl.clear(gl.COLOR_BUFFER_BIT)
    return null
  }
  const maxTextureSize = gl.getParameter(gl.MAX_TEXTURE_SIZE) as number
  const width = Math.min(maxTextureSize, Math.max(1, Math.ceil(Math.sqrt(nodeIndexById.size))))
  const height = Math.ceil(nodeIndexById.size / width)
  if (height > maxTextureSize) return replaceIndexedLayerWithCoordinate(state, canvas, onState)

  const positions = new Float32Array(width * height * 4)
  graph.forEachNode((node, attr) => {
    const index = nodeIndexById.get(node)
    if (index == null) return
    positions[index * 4] = (attr.x as number | undefined) ?? 0
    positions[index * 4 + 1] = (attr.y as number | undefined) ?? 0
  })
  const { data, edgeCount, length } = buildIndexedEdgeData(visibleEdges, nodeIndexById, hidden)
  gl.bindTexture(gl.TEXTURE_2D, state.positionTexture)
  gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1)
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA32F, width, height, 0, gl.RGBA, gl.FLOAT, positions)
  if (gl.getError() !== gl.NO_ERROR) {
    const fallback = replaceIndexedLayerWithCoordinate(state, canvas, onState)
    if (!fallback) clearLayer()
    return fallback
  }
  gl.bindBuffer(gl.ARRAY_BUFFER, state.edgeBuffer)
  gl.bufferData(
    gl.ARRAY_BUFFER,
    length === data.length ? data : data.subarray(0, length),
    gl.STATIC_DRAW,
  )
  const nextState = {
    ...state,
    nodeIndexById,
    positionTextureWidth: width,
    positionTextureHeight: height,
    positionTextureData: positions,
  }
  onState(nextState)
  onCounts(edgeCount)
  canvas.dataset.allEdgesCount = String(edgeCount)
  canvas.dataset.allEdgesLayer = 'indexed'
  if (edgeCount > 0) scheduleDraw()
  else clearWebGlCanvas(gl, canvas)
  return null
}

function replaceIndexedLayerWithCoordinate(
  state: AllEdgesIndexedLayerState,
  canvas: HTMLCanvasElement,
  onState: (state: AllEdgesLayerState | null) => void,
): AllEdgesLayerState | null {
  const gl = state.gl
  gl.deleteProgram(state.program)
  gl.deleteBuffer(state.edgeBuffer)
  gl.deleteTexture(state.positionTexture)
  const replacement = createCoordinateAllEdgesLayerState(canvas, gl)
  onState(replacement)
  return replacement
}

function clearWebGlCanvas(gl: WebGL2RenderingContext, canvas: HTMLCanvasElement): void {
  gl.viewport(0, 0, canvas.width, canvas.height)
  gl.clearColor(0, 0, 0, 0)
  gl.clear(gl.COLOR_BUFFER_BIT)
}

type OverlayDrawContext = {
  focus: NonNullable<NeighborhoodOverlayFocus>
  canvas: HTMLCanvasElement
  sigma: Sigma
  graph: Graph
  container: HTMLDivElement
  context: CanvasRenderingContext2D
  hidden: ReadonlySet<string> | null | undefined
  dragSource: { x: number; y: number } | null
  dragTargets: Array<{ x: number; y: number }> | null
  neighbors: ReadonlySet<string>
}

function resizeOverlayCanvas(
  canvas: HTMLCanvasElement,
  context: CanvasRenderingContext2D,
  container: HTMLDivElement,
): { width: number; height: number; pixelRatio: number } | null {
  const { width, height } = container.getBoundingClientRect()
  if (width <= 0 || height <= 0) return null
  const pixelRatio = Math.min(window.devicePixelRatio || 1, 2)
  const nextWidth = Math.max(1, Math.floor(width * pixelRatio))
  const nextHeight = Math.max(1, Math.floor(height * pixelRatio))
  if (canvas.width !== nextWidth || canvas.height !== nextHeight) {
    canvas.width = nextWidth
    canvas.height = nextHeight
  }
  canvas.style.width = `${width}px`
  canvas.style.height = `${height}px`
  context.setTransform(1, 0, 0, 1, 0, 0)
  context.clearRect(0, 0, canvas.width, canvas.height)
  context.setTransform(pixelRatio, 0, 0, pixelRatio, 0, 0)
  return { width, height, pixelRatio }
}

function resolveOverlaySource({ focus, dragSource, graph, sigma }: OverlayDrawContext) {
  if (dragSource) return dragSource
  const x = graph.getNodeAttribute(focus.nodeId, 'x') as number | undefined
  const y = graph.getNodeAttribute(focus.nodeId, 'y') as number | undefined
  return x == null || y == null ? null : sigma.graphToViewport({ x, y })
}

function drawOverlayEdges(draw: OverlayDrawContext, source: { x: number; y: number }): number {
  const { focus, context, dragTargets, neighbors, hidden, graph, sigma } = draw
  const limit =
    focus.mode === 'drag' ? DRAG_NEIGHBORHOOD_OVERLAY_EDGE_LIMIT : NEIGHBORHOOD_OVERLAY_EDGE_LIMIT
  let count = 0
  context.beginPath()
  if (dragTargets) {
    for (const target of dragTargets.slice(0, limit)) {
      context.moveTo(source.x, source.y)
      context.lineTo(target.x, target.y)
      count += 1
    }
    return count
  }
  for (const neighbor of neighbors) {
    if (count >= limit) break
    if (hidden?.has(neighbor) || !graph.hasNode(neighbor)) continue
    const x = graph.getNodeAttribute(neighbor, 'x') as number | undefined
    const y = graph.getNodeAttribute(neighbor, 'y') as number | undefined
    if (x == null || y == null) continue
    const target = sigma.graphToViewport({ x, y })
    context.moveTo(source.x, source.y)
    context.lineTo(target.x, target.y)
    count += 1
  }
  return count
}

function paintNeighborhoodOverlay(draw: OverlayDrawContext): boolean {
  if (!resizeOverlayCanvas(draw.canvas, draw.context, draw.container)) return false
  const source = resolveOverlaySource(draw)
  if (!source) return false
  const { stroke, halo } = overlayColors(draw.focus.mode)
  const context = draw.context
  context.save()
  context.lineCap = 'round'
  context.lineJoin = 'round'
  context.strokeStyle = stroke
  context.lineWidth = draw.focus.mode === 'hover' ? 1.05 : 1.55
  const edgeCount = drawOverlayEdges(draw, source)
  if (edgeCount > 0) context.stroke()
  context.fillStyle = halo
  context.beginPath()
  context.arc(source.x, source.y, draw.focus.mode === 'drag' ? 18 : 15, 0, Math.PI * 2)
  context.fill()
  context.strokeStyle =
    draw.focus.mode === 'hover' ? 'rgba(226, 232, 240, 0.78)' : 'rgba(251, 191, 36, 0.94)'
  context.lineWidth = 2
  context.beginPath()
  context.arc(source.x, source.y, draw.focus.mode === 'drag' ? 8 : 7, 0, Math.PI * 2)
  context.stroke()
  context.restore()
  draw.canvas.dataset.overlayNodeId = draw.focus.nodeId
  draw.canvas.dataset.overlayEdgeCount = String(edgeCount)
  draw.canvas.dataset.overlaySourceX = String(Math.round(source.x))
  draw.canvas.dataset.overlaySourceY = String(Math.round(source.y))
  return true
}

type EdgeSampleState = {
  graph: Graph
  edgeSet: Set<string>
  extraIds: Set<string>
  counts: Map<string, number>
  uncovered: Set<string>
  edgeSize: number
  edgeColor: string
  edgeType: string
  baseCount: number
}

function tryAddSampledEdge(state: EdgeSampleState, edge: EdgeData, isBase: boolean): boolean {
  if (!state.graph.hasNode(edge.sourceId) || !state.graph.hasNode(edge.targetId)) return false
  const key = edgePairKey(edge.sourceId, edge.targetId)
  if (state.edgeSet.has(key)) return false
  state.edgeSet.add(key)
  try {
    const edgeId = state.graph.addEdge(edge.sourceId, edge.targetId, {
      label: edge.label || '',
      size: state.edgeSize,
      color: state.edgeColor,
      type: state.edgeType,
    })
    if (!isBase) state.extraIds.add(edgeId)
    else recordBaseEdge(state, edge)
    return true
  } catch {
    return false
  }
}

function recordBaseEdge(state: EdgeSampleState, edge: EdgeData): void {
  state.baseCount += 1
  state.counts.set(edge.sourceId, (state.counts.get(edge.sourceId) ?? 0) + 1)
  state.counts.set(edge.targetId, (state.counts.get(edge.targetId) ?? 0) + 1)
  state.uncovered.delete(edge.sourceId)
  state.uncovered.delete(edge.targetId)
}

function addEdgesUntil(
  edges: EdgeData[],
  state: EdgeSampleState,
  limit: number,
  shouldAdd: (edge: EdgeData) => boolean,
  isBase: boolean,
): void {
  for (const edge of edges) {
    if ((isBase ? state.baseCount : state.edgeSet.size) >= limit) break
    if (shouldAdd(edge)) tryAddSampledEdge(state, edge, isBase)
  }
}

function sampleGraphEdges(
  graph: Graph,
  visibleEdges: EdgeData[],
  edgeSize: number,
  edgeColor: string,
  edgeType: string,
): Set<string> {
  const degrees = new Map<string, number>()
  const endpoints = new Set<string>()
  for (const edge of visibleEdges) {
    endpoints.add(edge.sourceId)
    endpoints.add(edge.targetId)
    degrees.set(edge.sourceId, (degrees.get(edge.sourceId) ?? 0) + 1)
    degrees.set(edge.targetId, (degrees.get(edge.targetId) ?? 0) + 1)
  }
  const defaultBudget = Math.min(
    visibleEdges.length,
    GRAPH_EDGE_DENSE_RENDER_CAP,
    Math.max(
      GRAPH_EDGE_RENDER_CAP,
      Math.ceil(endpoints.size * BASELINE_EDGE_ENDPOINT_COVERAGE_RATIO),
    ),
  )
  const coverageBudget = Math.min(
    defaultBudget,
    Math.ceil(defaultBudget * BASELINE_EDGE_ENDPOINT_COVERAGE_RATIO),
  )
  const detailBudget = Math.min(
    defaultBudget,
    Math.ceil(defaultBudget * BASELINE_EDGE_LOCAL_DETAIL_RATIO),
  )
  const denseBudget = Math.min(visibleEdges.length, GRAPH_EDGE_DENSE_RENDER_CAP)
  const defaultStride =
    visibleEdges.length > defaultBudget && defaultBudget > 0
      ? Math.ceil(visibleEdges.length / defaultBudget)
      : 1
  const denseStride =
    visibleEdges.length > denseBudget && denseBudget > 0
      ? Math.ceil(visibleEdges.length / denseBudget)
      : 1
  const state: EdgeSampleState = {
    graph,
    edgeSet: new Set(),
    extraIds: new Set(),
    counts: new Map(),
    uncovered: new Set(endpoints),
    edgeSize,
    edgeColor,
    edgeType,
    baseCount: 0,
  }
  const needsCoverage = (edge: EdgeData) =>
    state.uncovered.has(edge.sourceId) || state.uncovered.has(edge.targetId)
  const needsDetail = (id: string) =>
    (degrees.get(id) ?? 0) >= 4 &&
    (state.counts.get(id) ?? 0) < baselineEdgeSampleQuota(degrees.get(id) ?? 0)
  addEdgesUntil(visibleEdges, state, coverageBudget, needsCoverage, true)
  addEdgesUntil(
    visibleEdges,
    state,
    detailBudget,
    (edge) => needsDetail(edge.sourceId) || needsDetail(edge.targetId),
    true,
  )
  addEdgesUntil(
    visibleEdges,
    state,
    defaultBudget,
    (edge) => edgeSampleHash(edge) % defaultStride === 0,
    true,
  )
  addEdgesUntil(visibleEdges, state, defaultBudget, () => true, true)
  addEdgesUntil(
    visibleEdges,
    state,
    denseBudget,
    (edge) => edgeSampleHash(edge) % denseStride === 0,
    false,
  )
  addEdgesUntil(visibleEdges, state, denseBudget, () => true, false)
  return state.extraIds
}

function computeLodTiers(nodes: GraphNode[]) {
  const tiers = { overview: 0, mid: 0, near: 0 }
  if (nodes.length <= LOD_NODE_THRESHOLD) return tiers
  const degrees = nodes.map((node) => node.edgeCount).sort((a, b) => b - a)
  const threshold = (budget: number) => (budget >= degrees.length ? 0 : (degrees[budget] ?? 0))
  return {
    overview: threshold(LOD_TIER_BUDGETS.overview),
    mid: threshold(LOD_TIER_BUDGETS.mid),
    near: threshold(LOD_TIER_BUDGETS.near),
  }
}

function collectReusablePositions(
  graph: Graph | null,
  nodes: GraphNode[],
  canReuse: boolean,
): Map<string, { x: number; y: number }> {
  const positions = new Map<string, { x: number; y: number }>()
  if (!canReuse || !graph) return positions
  for (const node of nodes) {
    if (!graph.hasNode(node.id)) continue
    const x = graph.getNodeAttribute(node.id, 'x')
    const y = graph.getNodeAttribute(node.id, 'y')
    if (typeof x === 'number' && typeof y === 'number') positions.set(node.id, { x, y })
  }
  return positions
}

function populateGraphNodes(
  graph: Graph,
  nodes: GraphNode[],
  positions: ReadonlyMap<string, { x: number; y: number }>,
): void {
  const prominent =
    nodes.length > LABELS_DISABLED_NODE_THRESHOLD
      ? EMPTY_LABEL_SET
      : selectProminentGraphLabelIds(nodes)
  const sizeNode = buildNodeSizer(nodes)
  for (const node of nodes) {
    const showLabel = prominent.has(node.id)
    const label = showLabel ? buildGraphCanvasLabel(node.label, nodes.length) : ''
    graph.addNode(node.id, {
      label,
      displayLabel: label,
      originalLabel: node.label,
      focusLabel: buildGraphFocusLabel(node.label),
      x: positions.get(node.id)?.x ?? 0,
      y: positions.get(node.id)?.y ?? 0,
      size: sizeNode(node),
      color: GRAPH_NODE_COLORS[node.type] || GRAPH_NODE_COLORS.entity,
      nodeType: node.type,
      forceLabel: showLabel,
    })
  }
}

function existingGraphEdges(graph: Graph, edgeIds: string[]): string[] {
  return edgeIds.filter((edge) => graph.hasEdge(edge))
}

function resetSigmaCamera(sigma: Sigma, duration: number): void {
  // Camera reset is a visual enhancement. Keep the current view on failure,
  // but report it so rejected Sigma animations never disappear silently.
  void Promise.resolve(sigma.getCamera().animatedReset({ duration })).catch((error: unknown) => {
    console.error('[graph] camera reset failed', error)
  })
}

function isDenseEdgeRestoreCurrent({
  buildToken,
  sigmaForDrag,
  sigma,
  sigmaRef,
  graphRef,
  graph,
  node,
}: {
  buildToken: { cancelled: boolean }
  sigmaForDrag: Sigma | null
  sigma: Sigma
  sigmaRef: { current: Sigma | null }
  graphRef: { current: Graph | null }
  graph: Graph
  node: string
}): boolean {
  return (
    !buildToken.cancelled &&
    sigmaForDrag === sigma &&
    sigmaRef.current === sigma &&
    graphRef.current === graph &&
    graph.hasNode(node)
  )
}

function restoreDenseIncidentEdges({
  node,
  incidentEdges,
  sigma,
  graph,
  isCurrent,
  restoreLayers,
  frameRef,
}: {
  node: string
  incidentEdges: string[]
  sigma: Sigma
  graph: Graph
  isCurrent: () => boolean
  restoreLayers: () => void
  frameRef: { current: number | null }
}): void {
  if (!isCurrent()) {
    restoreLayers()
    return
  }

  const edgesToRefresh = existingGraphEdges(graph, incidentEdges)
  if (edgesToRefresh.length === 0) {
    sigma.refresh({
      partialGraph: { nodes: [node], edges: [] },
      schedule: true,
      skipIndexation: true,
    })
    restoreLayers()
    return
  }

  const restorer = createChunkedEdgeRestorer({
    edgeIds: edgesToRefresh,
    chunkSize: 180,
    isCurrent,
    refresh: (edgeChunk, includeNode) => {
      try {
        sigma.refresh({
          partialGraph: { nodes: includeNode ? [node] : [], edges: edgeChunk },
          schedule: true,
          skipIndexation: true,
        })
        return true
      } catch {
        sigma.refresh({ schedule: true, skipIndexation: true })
        return false
      }
    },
    restore: restoreLayers,
    schedule: (callback) => {
      frameRef.current = requestAnimationFrame(callback)
      return frameRef.current
    },
  })
  restorer.start()
}

function SigmaGraph({
  nodes,
  edges,
  selectedId,
  onSelect,
  layout,
  hiddenIds,
  onFitViewReady,
  showDenseEdges = false,
}: Readonly<SigmaGraphProps>) {
  const { t } = useTranslation()
  const preferences = useContext(PreferencesContext)
  const resolvedTheme =
    preferences?.resolvedTheme ??
    (typeof document !== 'undefined' && document.documentElement.classList.contains('dark')
      ? 'dark'
      : 'light')
  const containerRef = useRef<HTMLDivElement>(null)
  const tooltipRef = useRef<HTMLDivElement>(null)
  const sigmaRef = useRef<Sigma | null>(null)
  const [graphInstanceVersion, setGraphInstanceVersion] = useState(0)
  // Keep a stable ref to onFitViewReady so it can be read inside the
  // sigma-creation effect without appearing in the dependency array.
  // Adding the callback itself to deps would recreate the entire Sigma
  // instance on every parent render that passes a new function reference.
  const onFitViewReadyRef = useRef(onFitViewReady)
  useLayoutEffect(() => {
    onFitViewReadyRef.current = onFitViewReady
  })
  // Same stability requirement as `onFitViewReady`: the parent updates the URL
  // query string on selection, so its callback identity can change even though
  // Sigma's topology has not. Reading through a ref keeps selection from
  // tearing down/recreating the renderer and recentering the camera.
  const onSelectRef = useRef(onSelect)
  useLayoutEffect(() => {
    onSelectRef.current = onSelect
  })
  const graphRef = useRef<Graph | null>(null)
  const edgeLodExtraEdgeIdsRef = useRef<Set<string>>(new Set())
  const lastTopologyRef = useRef<{ nodes: GraphNode[]; edges: EdgeData[] } | null>(null)
  const lastCameraStateRef = useRef<CameraState | null>(null)
  const dragStateRef = useRef<{ dragging: boolean; node: string | null }>({
    dragging: false,
    node: null,
  })
  const selectedIdRef = useRef(selectedId)
  const [hoveredId, setHoveredId] = useState<string | null>(null)
  const layoutRef = useRef(layout)
  const layoutAnimationFrameRef = useRef<number | null>(null)
  const layoutAnimationTokenRef = useRef(0)
  // Monotonic token guarding async layout-switch recomputes. Bumped on
  // every switch so a stale worker result (user toggled twice quickly)
  // can be discarded before it touches Sigma.
  const layoutSwitchTokenRef = useRef(0)
  // Slim, transferable-friendly payload describing the CURRENT topology,
  // reused by the layout-switch effect so it never has to clone the live
  // Graphology instance (a ~770 ms main-thread stall at 100k nodes) just
  // to feed the layout worker. Rebuilt once per (nodes, edges) inside the
  // build effect, exactly mirroring the payload the initial render sends.
  const workerPayloadRef = useRef<{
    nodes: Array<{ id: string; nodeType: string; size: number; label: string }>
    edges: Array<{ sourceId: string; targetId: string }>
  } | null>(null)
  // Surfaced to a lightweight "recomputing layout" affordance while a
  // heavy async layout is in flight, so switching modes on a 100k-node
  // graph reads as deliberate rather than frozen.
  const [layoutRecomputing, setLayoutRecomputing] = useState(false)
  // Pre-computed `nodeId -> Set<neighborId>` lookup, rebuilt once per
  // (nodes, edges) change. The hover/click reducer used to call
  // `graph.neighbors(id)` on every effect run, which on a 25k-node graph
  // walks the full adjacency list each time. With a precomputed Map,
  // hover lookup becomes O(1). Built via useMemo so it only recomputes
  // when the input arrays actually change.
  const neighborIndex = useMemo(() => {
    const index = new Map<string, Set<string>>()
    for (const edge of edges) {
      if (edge.sourceId === edge.targetId) continue
      let outSet = index.get(edge.sourceId)
      if (!outSet) {
        outSet = new Set()
        index.set(edge.sourceId, outSet)
      }
      outSet.add(edge.targetId)
      let inSet = index.get(edge.targetId)
      if (!inSet) {
        inSet = new Set()
        index.set(edge.targetId, inSet)
      }
      inSet.add(edge.sourceId)
    }
    return index
  }, [edges])
  const neighborIndexRef = useRef(neighborIndex)
  useLayoutEffect(() => {
    neighborIndexRef.current = neighborIndex
  }, [neighborIndex])

  // --- Zoom + degree LOD (huge graphs only) --------------------------------
  // At overview, a huge graph renders only its most-connected hubs, so the view
  // reads as structure instead of an undifferentiated mass — and far fewer
  // nodes/edges paint per frame. Zooming in lowers the degree threshold and
  // progressively reveals the rest. Driven through the SAME hidden-set pipeline
  // as filters/search (`effectiveHiddenIds`), so the reducer and the WebGL edge
  // overlay apply it with no parallel code path. `node.edgeCount` is graph
  // degree (see graphAdapter).
  const [lodThreshold, setLodThreshold] = useState(0)
  const lodThresholdRef = useRef(0)
  useLayoutEffect(() => {
    lodThresholdRef.current = lodThreshold
  }, [lodThreshold])

  const lodHiddenIds = useMemo(() => {
    if (lodThreshold <= 0 || nodes.length <= LOD_NODE_THRESHOLD) return null
    const hidden = new Set<string>()
    for (const node of nodes) {
      if (node.id === selectedId) continue
      if (node.edgeCount < lodThreshold) hidden.add(node.id)
    }
    return hidden
  }, [nodes, lodThreshold, selectedId])

  const effectiveHiddenIds = useMemo(() => {
    if (!lodHiddenIds || lodHiddenIds.size === 0) return hiddenIds
    if (!hiddenIds || hiddenIds.size === 0) return lodHiddenIds
    const merged = new Set(hiddenIds)
    for (const id of lodHiddenIds) merged.add(id)
    return merged
  }, [hiddenIds, lodHiddenIds])

  const hiddenIdsRef = useRef(effectiveHiddenIds)
  useLayoutEffect(() => {
    hiddenIdsRef.current = effectiveHiddenIds
  }, [effectiveHiddenIds])

  useLayoutEffect(() => {
    selectedIdRef.current = selectedId
  }, [selectedId])

  // Cheap `nodeId -> label` lookup so the DOM tooltip can resolve names
  // without touching the Sigma graph instance. Built once per `nodes`
  // change, O(N) memory.
  const labelByNodeId = useMemo(() => {
    const map = new Map<string, string>()
    for (const n of nodes) map.set(n.id, n.label)
    return map
  }, [nodes])

  // Hidden-edge precompute. Owned by a ref that is rebuilt whenever the
  // graph rebuilds OR when `hiddenIds` changes. The reducer effect below
  // fires on every `hoveredId` change (once per intentional hover
  // commit); walking `graph.forEachEdge()` inside that effect would
  // repeatedly pay an O(M) scan on dense graphs where the user is
  // actively pointing. Precomputing once lets the reducer branches do an
  // O(1) `Set.has(edge)` check per edge per frame instead.
  const hiddenEdgeIdsRef = useRef<Set<string> | null>(null)

  // Tracks the node/edge ids that the PREVIOUS reducer run visually
  // touched (hovered/selected node + its neighbors + incident edges).
  // On the next hover/selection transition we partial-refresh the UNION
  // of {previously-affected} ∪ {newly-affected} so Sigma re-applies the
  // reducers to ~O(degree) elements instead of re-running them across
  // ALL nodes/edges. A full `sigma.refresh()` is O(N): on a 25k-node /
  // 160k-edge graph it blocks the main thread ~120 ms the instant the
  // cursor stops on a node, which is the perceived "freeze". The partial
  // path drops that to O(deg(hovered) + deg(prev)) — a handful of
  // updateNode/updateEdge calls — so the transition is imperceptible.
  const affectedNodeIdsRef = useRef<Set<string>>(new Set())
  const affectedEdgeIdsRef = useRef<Set<string>>(new Set())
  const renderedEdgeIdsRef = useRef<string[]>([])

  // DOM-only tooltip state. The card is anchored to the node's viewport
  // position (via `sigma.graphToViewport`), not to the cursor — so it
  // stays attached to the right node and never leaves a "tail" behind
  // when the cursor moves away. Position recomputed on hover commit and
  // on each Sigma camera update.
  const [tooltip, setTooltip] = useState<{
    nodeId: string
    label: string
    neighborLabels: string[]
    neighborCount: number
  } | null>(null)
  const [tooltipPos, setTooltipPos] = useState<{ x: number; y: number } | null>(null)
  const dragPreviewRef = useRef<HTMLDivElement>(null)
  const dragPreviewDotRef = useRef<HTMLSpanElement>(null)
  const dragPreviewLabelRef = useRef<HTMLSpanElement>(null)
  const allEdgesCanvasRef = useRef<HTMLCanvasElement>(null)
  const allEdgesLayerStateRef = useRef<AllEdgesLayerState | null>(null)
  const allEdgesLayerFrameRef = useRef<number | null>(null)
  const allEdgesLayerVertexCountRef = useRef(0)
  const allEdgesLayerEnabledRef = useRef(false)
  const suspendAllEdgesLayerAutoDrawRef = useRef(false)
  const suspendAllEdgesLayerOwnerTokenRef = useRef<number | null>(null)
  const neighborhoodCanvasRef = useRef<HTMLCanvasElement>(null)
  const neighborhoodOverlayFocusRef = useRef<NeighborhoodOverlayFocus>(null)
  const neighborhoodOverlayFrameRef = useRef<number | null>(null)
  const dragOverlayTargetsRef = useRef<Array<{ x: number; y: number }> | null>(null)
  const dragOverlaySourceRef = useRef<{ x: number; y: number } | null>(null)
  const clearAllEdgesLayer = useCallback(() => {
    allEdgesLayerEnabledRef.current = false
    const canvas = allEdgesCanvasRef.current
    const state = allEdgesLayerStateRef.current
    if (!canvas || !state) return
    allEdgesLayerVertexCountRef.current = 0
    const gl = state.gl
    gl.viewport(0, 0, canvas.width, canvas.height)
    gl.clearColor(0, 0, 0, 0)
    gl.clear(gl.COLOR_BUFFER_BIT)
    delete canvas.dataset.allEdgesCount
    delete canvas.dataset.allEdgesLayer
  }, [])
  // Explicitly free the all-edges layer's GL objects and drop the cached
  // state. Called on unmount (below) so repeated GraphPage mount/unmount
  // cycles never leak a WebGL context toward the browser's ~16-context
  // ceiling — the leak that eventually forces a context loss with no
  // recovery path.
  const disposeAllEdgesLayerGl = useCallback(() => {
    const state = allEdgesLayerStateRef.current
    allEdgesLayerStateRef.current = null
    allEdgesLayerEnabledRef.current = false
    allEdgesLayerVertexCountRef.current = 0
    if (!state) return
    const gl = state.gl
    try {
      gl.deleteProgram(state.program)
      if (state.kind === 'indexed') {
        gl.deleteBuffer(state.edgeBuffer)
        gl.deleteTexture(state.positionTexture)
      } else {
        gl.deleteBuffer(state.buffer)
      }
    } catch {
      // Context already lost — the GL objects are gone; nothing to free.
    }
  }, [])
  const drawAllEdgesLayerNow = useCallback(() => {
    const canvas = allEdgesCanvasRef.current
    const sigma = sigmaRef.current
    const container = containerRef.current
    const state = allEdgesLayerStateRef.current
    const vertexCount = allEdgesLayerVertexCountRef.current
    if (
      !canvas ||
      !sigma ||
      !container ||
      !state ||
      !allEdgesLayerEnabledRef.current ||
      vertexCount <= 0
    ) {
      return
    }

    const rect = container.getBoundingClientRect()
    if (rect.width <= 0 || rect.height <= 0) return

    const pixelRatio = Math.min(window.devicePixelRatio || 1, 2)
    const nextWidth = Math.max(1, Math.floor(rect.width * pixelRatio))
    const nextHeight = Math.max(1, Math.floor(rect.height * pixelRatio))
    if (canvas.width !== nextWidth || canvas.height !== nextHeight) {
      canvas.width = nextWidth
      canvas.height = nextHeight
    }
    canvas.style.width = `${rect.width}px`
    canvas.style.height = `${rect.height}px`

    const origin = sigma.graphToViewport({ x: 0, y: 0 })
    const unitX = sigma.graphToViewport({ x: 1, y: 0 })
    const unitY = sigma.graphToViewport({ x: 0, y: 1 })
    const a = unitX.x - origin.x
    const b = unitX.y - origin.y
    const c = unitY.x - origin.x
    const d = unitY.y - origin.y
    const e = origin.x
    const f = origin.y
    const matrix = new Float32Array([
      (2 * a) / rect.width,
      (-2 * b) / rect.height,
      0,
      (2 * c) / rect.width,
      (-2 * d) / rect.height,
      0,
      (2 * e) / rect.width - 1,
      1 - (2 * f) / rect.height,
      1,
    ])
    const color =
      resolvedTheme === 'dark' ? ALL_EDGES_LAYER_DARK_COLOR : ALL_EDGES_LAYER_LIGHT_COLOR
    const gl = state.gl
    gl.viewport(0, 0, canvas.width, canvas.height)
    gl.clearColor(0, 0, 0, 0)
    gl.clear(gl.COLOR_BUFFER_BIT)
    gl.useProgram(state.program)
    if (state.kind === 'indexed') {
      gl.activeTexture(gl.TEXTURE0)
      gl.bindTexture(gl.TEXTURE_2D, state.positionTexture)
      gl.uniform1i(state.positionTextureLocation, 0)
      gl.uniform1f(state.positionTextureWidthLocation, state.positionTextureWidth)
      gl.bindBuffer(gl.ARRAY_BUFFER, state.edgeBuffer)
      gl.enableVertexAttribArray(state.edgeDataLocation)
      gl.vertexAttribPointer(state.edgeDataLocation, 3, gl.FLOAT, false, 0, 0)
    } else {
      gl.bindBuffer(gl.ARRAY_BUFFER, state.buffer)
      gl.enableVertexAttribArray(state.positionLocation)
      gl.vertexAttribPointer(state.positionLocation, 2, gl.FLOAT, false, 0, 0)
    }
    gl.uniformMatrix3fv(state.matrixLocation, false, matrix)
    gl.uniform4f(state.colorLocation, color[0], color[1], color[2], color[3])
    gl.drawArrays(gl.LINES, 0, vertexCount)
  }, [resolvedTheme])
  const scheduleAllEdgesLayerDraw = useCallback(() => {
    if (suspendAllEdgesLayerAutoDrawRef.current) return
    if (allEdgesLayerFrameRef.current != null) return
    allEdgesLayerFrameRef.current = requestAnimationFrame(() => {
      allEdgesLayerFrameRef.current = null
      drawAllEdgesLayerNow()
    })
  }, [drawAllEdgesLayerNow])
  const suspendAllEdgesLayerDraws = useCallback((token: number) => {
    suspendAllEdgesLayerAutoDrawRef.current = true
    suspendAllEdgesLayerOwnerTokenRef.current = token
    if (allEdgesLayerFrameRef.current != null) {
      cancelAnimationFrame(allEdgesLayerFrameRef.current)
      allEdgesLayerFrameRef.current = null
    }
    const canvas = allEdgesCanvasRef.current
    if (canvas) canvas.style.visibility = 'hidden'
  }, [])
  const resumeAllEdgesLayerDraws = useCallback(
    (token: number, drawFresh: boolean) => {
      if (suspendAllEdgesLayerOwnerTokenRef.current !== token) return false
      suspendAllEdgesLayerOwnerTokenRef.current = null
      suspendAllEdgesLayerAutoDrawRef.current = false
      if (drawFresh) drawAllEdgesLayerNow()
      const canvas = allEdgesCanvasRef.current
      if (canvas) canvas.style.visibility = ''
      return true
    },
    [drawAllEdgesLayerNow],
  )
  const forceClearAllEdgesLayerSuspension = useCallback(() => {
    suspendAllEdgesLayerOwnerTokenRef.current = null
    suspendAllEdgesLayerAutoDrawRef.current = false
    if (allEdgesLayerFrameRef.current != null) {
      cancelAnimationFrame(allEdgesLayerFrameRef.current)
      allEdgesLayerFrameRef.current = null
    }
    const canvas = allEdgesCanvasRef.current
    if (canvas) canvas.style.visibility = ''
  }, [])
  const rebuildAllEdgesLayer = useCallback(
    (graph: Graph, visibleEdges: EdgeData[], enabled: boolean) => {
      const canvas = allEdgesCanvasRef.current
      if (!canvas || !enabled) {
        clearAllEdgesLayer()
        return
      }

      let state = allEdgesLayerStateRef.current
      if (!state) {
        state = createAllEdgesLayerState(canvas)
        allEdgesLayerStateRef.current = state
      }
      if (!state) return

      const hidden = hiddenIdsRef.current

      if (state.kind === 'indexed') {
        const setState = (next: AllEdgesLayerState | null) => {
          allEdgesLayerStateRef.current = next
        }
        const setCounts = (edgeCount: number) => {
          allEdgesLayerVertexCountRef.current = edgeCount * 2
          allEdgesLayerEnabledRef.current = edgeCount > 0
        }
        const fallback = uploadIndexedAllEdgesLayer({
          state,
          graph,
          visibleEdges,
          hidden,
          canvas,
          onState: setState,
          onCounts: setCounts,
          scheduleDraw: scheduleAllEdgesLayerDraw,
          clearLayer: clearAllEdgesLayer,
        })
        if (!fallback) return
        state = fallback
      }

      if (state.kind !== 'coordinate') return
      const edgeCount = uploadCoordinateAllEdgesLayer(
        state,
        graph,
        visibleEdges,
        hidden,
        canvas,
        scheduleAllEdgesLayerDraw,
      )
      allEdgesLayerVertexCountRef.current = edgeCount * 2
      allEdgesLayerEnabledRef.current = edgeCount > 0
    },
    [clearAllEdgesLayer, scheduleAllEdgesLayerDraw],
  )
  // Kept current so the mount-once context-loss effect can rebuild the layer
  // on `webglcontextrestored` without listing `rebuildAllEdgesLayer` in its
  // deps (which would re-run — and dispose a healthy layer — on every theme
  // toggle, since that callback's identity changes with `resolvedTheme`).
  const rebuildAllEdgesLayerRef = useRef(rebuildAllEdgesLayer)
  useLayoutEffect(() => {
    rebuildAllEdgesLayerRef.current = rebuildAllEdgesLayer
  })
  const updateAllEdgesLayerNodePosition = useCallback(
    (node: string, position: { x: number; y: number }) => {
      const state = allEdgesLayerStateRef.current
      if (state?.kind !== 'indexed' || !allEdgesLayerEnabledRef.current) return false
      const nodeIndex = state.nodeIndexById.get(node)
      if (nodeIndex == null) return false

      const offset = nodeIndex * 4
      state.positionTextureData[offset] = position.x
      state.positionTextureData[offset + 1] = position.y
      state.scratchTexel[0] = position.x
      state.scratchTexel[1] = position.y
      state.scratchTexel[2] = 0
      state.scratchTexel[3] = 0

      const x = nodeIndex % state.positionTextureWidth
      const y = Math.floor(nodeIndex / state.positionTextureWidth)
      const gl = state.gl
      gl.bindTexture(gl.TEXTURE_2D, state.positionTexture)
      gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1)
      gl.texSubImage2D(gl.TEXTURE_2D, 0, x, y, 1, 1, gl.RGBA, gl.FLOAT, state.scratchTexel)
      scheduleAllEdgesLayerDraw()
      return true
    },
    [scheduleAllEdgesLayerDraw],
  )
  const uploadAllEdgesLayerPositionTexture = useCallback(
    (state: AllEdgesIndexedLayerState) => {
      const gl = state.gl
      gl.bindTexture(gl.TEXTURE_2D, state.positionTexture)
      gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1)
      gl.texSubImage2D(
        gl.TEXTURE_2D,
        0,
        0,
        0,
        state.positionTextureWidth,
        state.positionTextureHeight,
        gl.RGBA,
        gl.FLOAT,
        state.positionTextureData,
      )
      scheduleAllEdgesLayerDraw()
      return true
    },
    [scheduleAllEdgesLayerDraw],
  )
  const uploadAllEdgesLayerPositionTextureChunked = useCallback(
    async (state: AllEdgesIndexedLayerState, isCurrent: () => boolean) => {
      const gl = state.gl
      const rowsPerChunk = state.positionTextureHeight >= 64 ? 4 : state.positionTextureHeight
      gl.bindTexture(gl.TEXTURE_2D, state.positionTexture)
      gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1)

      for (let row = 0; row < state.positionTextureHeight; row += rowsPerChunk) {
        if (!isCurrent() || allEdgesLayerStateRef.current !== state) return false
        const rowCount = Math.min(rowsPerChunk, state.positionTextureHeight - row)
        const start = row * state.positionTextureWidth * 4
        const end = (row + rowCount) * state.positionTextureWidth * 4
        gl.texSubImage2D(
          gl.TEXTURE_2D,
          0,
          0,
          row,
          state.positionTextureWidth,
          rowCount,
          gl.RGBA,
          gl.FLOAT,
          state.positionTextureData.subarray(start, end),
        )
        await waitForAnimationFrame()
      }

      if (!isCurrent() || allEdgesLayerStateRef.current !== state) return false
      scheduleAllEdgesLayerDraw()
      return true
    },
    [scheduleAllEdgesLayerDraw],
  )
  const syncAllEdgesLayerPositionsFromGraph = useCallback(
    (graph: Graph, enabled: boolean) => {
      const state = allEdgesLayerStateRef.current
      if (!enabled || state?.kind !== 'indexed' || !allEdgesLayerEnabledRef.current) return false

      for (const [node, nodeIndex] of state.nodeIndexById) {
        if (!graph.hasNode(node)) continue
        const x = graph.getNodeAttribute(node, 'x') as number | undefined
        const y = graph.getNodeAttribute(node, 'y') as number | undefined
        if (x == null || y == null) continue
        const offset = nodeIndex * 4
        state.positionTextureData[offset] = x
        state.positionTextureData[offset + 1] = y
      }

      return uploadAllEdgesLayerPositionTexture(state)
    },
    [uploadAllEdgesLayerPositionTexture],
  )
  const writeAllEdgesLayerPositionsFromLayoutChunked = useCallback(
    async (
      layoutNodes: Array<{ id: string }>,
      positions: ArrayLike<number>,
      enabled: boolean,
      isCurrent: () => boolean,
    ) => {
      const state = allEdgesLayerStateRef.current
      if (!enabled || state?.kind !== 'indexed' || !allEdgesLayerEnabledRef.current) return null
      const chunkSize = layoutNodes.length >= 50000 ? 1000 : 5000

      const isStateCurrent = () => isCurrent() && allEdgesLayerStateRef.current === state
      return updatePositionTextureFromLayoutChunked({
        positionTextureData: state.positionTextureData,
        nodeIndexById: state.nodeIndexById,
        layoutNodes,
        positions,
        chunkSize,
        isCurrent: isStateCurrent,
        waitForFrame: waitForAnimationFrame,
      }).then((updated) => (updated ? state : null))
    },
    [],
  )
  const syncOrRebuildAllEdgesLayerPositions = useCallback(
    (graph: Graph, visibleEdges: EdgeData[], enabled: boolean) => {
      if (syncAllEdgesLayerPositionsFromGraph(graph, enabled)) return
      rebuildAllEdgesLayer(graph, visibleEdges, enabled)
    },
    [rebuildAllEdgesLayer, syncAllEdgesLayerPositionsFromGraph],
  )
  const clearNeighborhoodOverlay = useCallback(() => {
    const canvas = neighborhoodCanvasRef.current
    if (!canvas) return
    delete canvas.dataset.overlayNodeId
    delete canvas.dataset.overlayEdgeCount
    delete canvas.dataset.overlaySourceX
    delete canvas.dataset.overlaySourceY
    const context = getCanvas2dContext(canvas)
    if (!context) return
    context.setTransform(1, 0, 0, 1, 0, 0)
    context.clearRect(0, 0, canvas.width, canvas.height)
  }, [])
  const drawNeighborhoodOverlayNow = useCallback(() => {
    const focus = neighborhoodOverlayFocusRef.current
    const canvas = neighborhoodCanvasRef.current
    const sigma = sigmaRef.current
    const graph = graphRef.current
    const container = containerRef.current
    if (!focus || !canvas || !sigma || !graph || !container || !graph.hasNode(focus.nodeId)) {
      clearNeighborhoodOverlay()
      return
    }
    const context = getCanvas2dContext(canvas)
    if (!context) return
    const painted = paintNeighborhoodOverlay({
      focus,
      canvas,
      sigma,
      graph,
      container,
      context,
      hidden: hiddenIdsRef.current,
      dragSource: focus.mode === 'drag' ? dragOverlaySourceRef.current : null,
      dragTargets: focus.mode === 'drag' ? dragOverlayTargetsRef.current : null,
      neighbors: neighborIndexRef.current.get(focus.nodeId) ?? new Set<string>(),
    })
    if (!painted) clearNeighborhoodOverlay()
  }, [clearNeighborhoodOverlay])

  const scheduleNeighborhoodOverlayDraw = useCallback(() => {
    if (neighborhoodOverlayFrameRef.current != null) return
    neighborhoodOverlayFrameRef.current = requestAnimationFrame(() => {
      neighborhoodOverlayFrameRef.current = null
      drawNeighborhoodOverlayNow()
    })
  }, [drawNeighborhoodOverlayNow])
  const hideDragPreview = useCallback(() => {
    const preview = dragPreviewRef.current
    if (!preview) return
    preview.hidden = true
    preview.style.visibility = 'hidden'
    preview.style.transform = 'translate3d(-9999px, -9999px, 0)'
    delete preview.dataset.dragNodeId
  }, [])
  useEffect(() => {
    return () => {
      if (allEdgesLayerFrameRef.current != null) {
        cancelAnimationFrame(allEdgesLayerFrameRef.current)
        allEdgesLayerFrameRef.current = null
      }
      if (neighborhoodOverlayFrameRef.current != null) {
        cancelAnimationFrame(neighborhoodOverlayFrameRef.current)
        neighborhoodOverlayFrameRef.current = null
      }
    }
  }, [])

  // WebGL context-loss recovery + explicit teardown for the all-edges overlay
  // canvas. Without this, (1) a lost context (GPU TDR / tab eviction) leaves
  // the dense-graph edge layer permanently blank because `rebuildAllEdgesLayer`
  // only creates state when it is `null`, and the ref is never nulled on loss;
  // and (2) repeated GraphPage mount/unmount cycles never free the context, so
  // they accumulate toward the browser's ~16-context ceiling and start evicting
  // live contexts. Mount-once: the handlers read live refs, so the effect never
  // needs to re-run — and must not, since re-running would dispose a healthy
  // layer.
  useEffect(() => {
    const canvas = allEdgesCanvasRef.current
    if (!canvas) return
    const handleContextLost = (event: Event) => {
      // preventDefault signals the browser we intend to restore. The GL
      // objects are already invalid; drop the state so the next rebuild
      // recreates them from scratch.
      event.preventDefault()
      allEdgesLayerStateRef.current = null
      allEdgesLayerEnabledRef.current = false
      allEdgesLayerVertexCountRef.current = 0
      if (allEdgesLayerFrameRef.current != null) {
        cancelAnimationFrame(allEdgesLayerFrameRef.current)
        allEdgesLayerFrameRef.current = null
      }
    }
    const handleContextRestored = () => {
      const graph = graphRef.current
      const topology = lastTopologyRef.current
      if (!graph || !topology) return
      const useAllEdgesLayer =
        topology.nodes.length >= ALL_EDGES_LAYER_NODE_THRESHOLD ||
        topology.edges.length > ALL_EDGES_LAYER_EDGE_THRESHOLD
      rebuildAllEdgesLayerRef.current(graph, topology.edges, useAllEdgesLayer)
    }
    canvas.addEventListener('webglcontextlost', handleContextLost)
    canvas.addEventListener('webglcontextrestored', handleContextRestored)
    return () => {
      canvas.removeEventListener('webglcontextlost', handleContextLost)
      canvas.removeEventListener('webglcontextrestored', handleContextRestored)
      disposeAllEdgesLayerGl()
    }
  }, [disposeAllEdgesLayerGl])

  useEffect(() => {
    const useDomOnlyInteractions = nodes.length >= DOM_ONLY_INTERACTION_NODE_THRESHOLD
    if (!useDomOnlyInteractions) {
      neighborhoodOverlayFocusRef.current = null
      clearNeighborhoodOverlay()
      return
    }

    const current = neighborhoodOverlayFocusRef.current
    if (current?.mode === 'drag') {
      scheduleNeighborhoodOverlayDraw()
      return
    }

    if (selectedId && graphRef.current?.hasNode(selectedId)) {
      neighborhoodOverlayFocusRef.current = { nodeId: selectedId, mode: 'selected' }
    } else if (hoveredId && graphRef.current?.hasNode(hoveredId)) {
      neighborhoodOverlayFocusRef.current = { nodeId: hoveredId, mode: 'hover' }
    } else {
      neighborhoodOverlayFocusRef.current = null
    }
    scheduleNeighborhoodOverlayDraw()
  }, [
    clearNeighborhoodOverlay,
    graphInstanceVersion,
    hiddenIds,
    hoveredId,
    nodes.length,
    scheduleNeighborhoodOverlayDraw,
    selectedId,
  ])
  useLayoutEffect(() => {
    hideDragPreview()
  }, [hideDragPreview])
  // **Dwell-time hover**. The hover state only commits after the cursor
  // has been on the same node for `HOVER_DWELL_MS`. Fast sweeps across a
  // dense graph never commit, so they cost nothing — we never run the
  // expensive Sigma reducer + refresh path until the user actually
  // *stops* to look at a node. Tooltip + card show immediately though,
  // independent of dwell, since they live outside Sigma.
  const HOVER_DWELL_MS = 140
  const pendingHoverRef = useRef<string | null>(null)
  const hoverTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const scheduleHoverUpdate = (next: string | null) => {
    pendingHoverRef.current = next
    if (hoverTimerRef.current != null) {
      clearTimeout(hoverTimerRef.current)
      hoverTimerRef.current = null
    }
    // Clearing hover (leaveNode) is immediate: no dwell wait.
    if (next == null) {
      setHoveredId((current) => (current == null ? current : null))
      return
    }
    hoverTimerRef.current = setTimeout(() => {
      hoverTimerRef.current = null
      setHoveredId((current) =>
        current === pendingHoverRef.current ? current : pendingHoverRef.current,
      )
    }, HOVER_DWELL_MS)
  }

  // Build the floating tooltip only when `hoveredId` COMMITS (after the dwell
  // gate) — never on the raw per-node `enterNode` stream. Fast cursor sweeps
  // across a dense cluster therefore queue zero tooltip re-renders; the card
  // materialises once the cursor actually rests. The sigma camera handler
  // refines its position on pan/zoom by reading the rendered card's
  // data-node-id, so this effect only needs to seed the initial anchor.
  useEffect(() => {
    const sigma = sigmaRef.current
    const graph = graphRef.current
    if (!hoveredId || !sigma || !graph?.hasNode(hoveredId)) {
      setTooltip(null)
      setTooltipPos(null)
      return
    }
    const neighborSet = neighborIndex.get(hoveredId)
    const label =
      labelByNodeId.get(hoveredId) ??
      (graph.getNodeAttribute(hoveredId, 'originalLabel') as string | undefined) ??
      hoveredId
    setTooltip(
      buildTooltipData({
        nodeId: hoveredId,
        neighbors: neighborSet,
        labelByNodeId,
        nodeLabel: label,
        maxNeighbors: 12,
      }),
    )
    const x = graph.getNodeAttribute(hoveredId, 'x') as number | undefined
    const y = graph.getNodeAttribute(hoveredId, 'y') as number | undefined
    if (x != null && y != null) {
      const viewport = sigma.graphToViewport({ x, y })
      const containerRect = containerRef.current?.getBoundingClientRect()
      setTooltipPos({
        x: viewport.x + (containerRect?.left ?? 0),
        y: viewport.y + (containerRect?.top ?? 0),
      })
    }
  }, [hoveredId, neighborIndex, labelByNodeId])

  const stopLayoutAnimation = () => {
    layoutAnimationTokenRef.current += 1
    if (layoutAnimationFrameRef.current != null) {
      cancelAnimationFrame(layoutAnimationFrameRef.current)
      layoutAnimationFrameRef.current = null
    }
  }

  useEffect(() => {
    if (!containerRef.current || nodes.length === 0) return

    // Cancellation gate. The build path can be async (Web Worker
    // layout), so if the effect is re-run (topology change, layout
    // change, unmount) before the worker resolves, we must abort the
    // half-built state instead of creating a zombie Sigma instance.
    const buildToken = { cancelled: false }
    stopLayoutAnimation()
    const previousGraph = graphRef.current
    const canReuseLayout =
      previousGraph != null &&
      lastTopologyRef.current?.nodes === nodes &&
      lastTopologyRef.current?.edges === edges
    const reusedPositions = collectReusablePositions(previousGraph, nodes, canReuseLayout)
    const reuseLayout = canReuseLayout && reusedPositions.size === nodes.length
    const reusedCameraState = reuseLayout ? lastCameraStateRef.current : null
    const graph = new Graph()

    const visibleNodes = nodes
    const visibleNodeIds = new Set(visibleNodes.map((n) => n.id))
    const visibleEdges = edges.filter(
      (edge) =>
        edge.sourceId !== edge.targetId &&
        visibleNodeIds.has(edge.sourceId) &&
        visibleNodeIds.has(edge.targetId),
    )
    const denseGraph = visibleEdges.length > 2200 || visibleNodes.length > 700
    // LOD degree thresholds for this graph's degree distribution: pick the
    // degree at each visible-node budget so the overview backbone stays a
    // stable size regardless of the graph's shape. Zero disables LOD.
    const lodTiers = computeLodTiers(visibleNodes)
    const useDomOnlyInteractions = visibleNodes.length >= DOM_ONLY_INTERACTION_NODE_THRESHOLD
    const useAllEdgesLayer =
      visibleNodes.length >= ALL_EDGES_LAYER_NODE_THRESHOLD ||
      visibleEdges.length > ALL_EDGES_LAYER_EDGE_THRESHOLD
    const { color: edgeColor, size: edgeSize } = denseEdgeStyle(denseGraph, resolvedTheme)
    const labelDensity = labelDensityForNodeCount(visibleNodes.length)
    const defaultEdgeType = denseGraph ? 'line' : 'curvedArrow'
    populateGraphNodes(graph, visibleNodes, reusedPositions)

    // Edge-render LOD: build the dense sample once, then hide/show the extra
    // edge ids through the reducer. This makes the toolbar density toggle a
    // partial repaint instead of changing the graph's edge cardinality, which
    // would force Sigma to run a full `process()` and hitch Firefox.
    const edgeLodExtraEdgeIds = sampleGraphEdges(
      graph,
      visibleEdges,
      edgeSize,
      edgeColor,
      defaultEdgeType,
    )
    edgeLodExtraEdgeIdsRef.current = edgeLodExtraEdgeIds
    renderedEdgeIdsRef.current = graph.edges()

    // Slim payload describing this topology. Built ONCE here and stashed
    // in `workerPayloadRef` so the layout-switch effect can re-run the
    // worker without ever cloning the live Graphology instance — the
    // single most expensive op on the old switch path (~770 ms at 100k
    // nodes). The worker reads `{id, nodeType, size, label}` per node and
    // `{sourceId, targetId}` per edge; positions come back as a
    // transferable Float32Array (zero-copy).
    const workerNodes = visibleNodes.map((node) => ({
      id: node.id,
      nodeType: node.type,
      size: (graph.getNodeAttribute(node.id, 'size') as number | undefined) ?? 1,
      label: node.label,
    }))
    const workerEdges = visibleEdges.map((edge) => ({
      sourceId: edge.sourceId,
      targetId: edge.targetId,
      weight: edge.weight,
    }))
    const workerPayload = { nodes: workerNodes, edges: workerEdges }
    workerPayloadRef.current = workerPayload

    // Compute the INITIAL layout either synchronously or off-main-thread.
    // The worker path avoids a second main-thread Graphology build and
    // keeps the expensive force/component math off the critical frame
    // path. For graphs below `GRAPH_WORKER_NODE_THRESHOLD` the sync
    // codepath wins because the postMessage round-trip is pure overhead —
    // EXCEPT for iterative (force) layouts, which must always run in the
    // worker regardless of size or their FA2 passes would stall the main
    // thread (the sync path only produces the geometric seed).
    const useWorker =
      visibleNodes.length >= GRAPH_WORKER_NODE_THRESHOLD || isIterativeLayout(layout)
    let layoutComputation: Promise<void>
    if (reuseLayout) {
      layoutComputation = Promise.resolve()
    } else if (useWorker) {
      layoutComputation = (async () => {
        try {
          const result = await computeGraphLayoutOffThread({
            nodes: workerNodes,
            edges: workerEdges,
            layout,
            cacheKey: workerPayload,
          })
          if (buildToken.cancelled) return
          // Bulk-apply via `updateEachNodeAttributes` (one traversal,
          // mutating x/y in place) instead of 2N individual
          // `setNodeAttribute` calls — the latter each emit a Graphology
          // event Sigma would otherwise react to. Sigma is not attached
          // yet here, but the bulk form is still markedly cheaper.
          const positionById = new Map<string, number>()
          for (let i = 0; i < workerNodes.length; i += 1) {
            const workerNode = workerNodes[i]
            if (!workerNode) continue
            positionById.set(workerNode.id, i)
          }
          graph.updateEachNodeAttributes(
            (id, attr) => {
              const i = positionById.get(id)
              if (i != null) {
                attr.x = result.positions[i * 2]
                attr.y = result.positions[i * 2 + 1]
              }
              return attr
            },
            { attributes: ['x', 'y'] },
          )
        } catch (error) {
          // Worker failed (bundler misconfig, OOM, whatever). Do not run
          // the dense layout on the main thread: a simple O(N) fallback is
          // enough to keep the graph visible without a multi-frame stall.
          if (buildToken.cancelled) return
          console.warn('[graph] worker layout failed, using cheap fallback layout', error)
          applyCheapFallbackLayout(graph)
        }
      })()
    } else {
      layoutComputation = Promise.resolve().then(() => {
        applyGraphLayout(graph, layout)
      })
    }

    let sigmaInstance: Sigma | null = null
    let denseBaseEdgeLayersHidden = false
    const denseEdgeRestoreFrame = { current: null as number | null }
    const restoreSigmaNodeLayers = () => {
      const sigmaForLayers = sigmaInstance
      if (!sigmaForLayers) return
      const canvases = sigmaForLayers.getCanvases()
      for (const layer of SIGMA_NODE_CANVAS_LAYERS) {
        const canvas = canvases[layer]
        if (canvas) canvas.style.visibility = ''
      }
    }
    const setAllEdgesLayerHidden = (hidden: boolean) => {
      const canvas = allEdgesCanvasRef.current
      if (canvas) canvas.style.visibility = hidden ? 'hidden' : ''
    }
    const setDenseBaseEdgeLayersHidden = (hidden: boolean) => {
      restoreSigmaNodeLayers()
      if (denseBaseEdgeLayersHidden === hidden) return
      denseBaseEdgeLayersHidden = hidden
      const sigmaForLayers = sigmaInstance
      if (!sigmaForLayers) return
      const canvases = sigmaForLayers.getCanvases()
      for (const layer of ['edges', 'edgeLabels'] as const) {
        const canvas = canvases[layer]
        if (canvas) canvas.style.visibility = hidden ? 'hidden' : ''
      }
    }
    const restoreDenseBaseEdgeLayers = () => {
      setDenseBaseEdgeLayersHidden(useAllEdgesLayer && allEdgesLayerEnabledRef.current)
    }

    // Keep the full-topology edge overlay LIVE while the camera pans/zooms so
    // the links follow the nodes instead of vanishing mid-drag. LOD already
    // thins the overlay to the visible hub backbone on large graphs, so the
    // per-frame WebGL LINES redraw stays cheap; the draw itself is rAF-throttled
    // by `scheduleAllEdgesLayerDraw`.
    const onCameraMoveEdges = () => {
      if (!useAllEdgesLayer || suspendAllEdgesLayerAutoDrawRef.current) return
      scheduleAllEdgesLayerDraw()
    }

    let lodScheduler: ReturnType<typeof createLodScheduler> | null = null
    void layoutComputation.then(() => {
      if (buildToken.cancelled) return
      if (!containerRef.current) return
      layoutRef.current = layout

      graphRef.current = graph
      if (sigmaRef.current) sigmaRef.current.kill()

      // Label-system tuning by graph density. The collision detection Sigma
      // runs for label placement is the dominant cost per frame on dense
      // graphs, and the thresholds below raise the bar on "is this node
      // large enough to deserve a label check at all" so the expensive
      // pass runs on far fewer nodes. `labelGridCellSize` tunes the spatial
      // hash used for label collisions — bigger cells = fewer cells =
      // cheaper lookup, at the cost of slightly looser deduplication.
      const ultraDenseGraph = visibleNodes.length > 5000
      const labelsDisabled = visibleNodes.length > LABELS_DISABLED_NODE_THRESHOLD
      const labelRenderedSizeThreshold = labelRenderedThreshold(visibleNodes.length, labelsDisabled)
      const labelGridCellSize = visibleNodes.length > 5000 ? 240 : 100

      // `hideEdgesOnMove` is gated on the RENDERED (post-cap) edge count, not the
      // raw total. With the edge cap active a GPU renders the sampled edge set at
      // full frame rate even during a camera move, so hiding edges mid-move would
      // only add a visible repaint hitch when the gesture ends (Sigma repaints
      // every edge once on release). We therefore keep edges visible while moving
      // for capped graphs and only hide them when the rendered edge set is truly
      // huge, where the per-frame edge pass would otherwise blow the frame
      // budget.
      const renderedEdgeCount = Math.min(visibleEdges.length, GRAPH_EDGE_DENSE_RENDER_CAP)
      const hideEdgesWhileMoving = renderedEdgeCount > 120000

      const sigma = new Sigma(graph, containerRef.current, {
        hideEdgesOnMove: hideEdgesWhileMoving,
        // On dense graphs, labels are skipped entirely during pan/zoom to
        // keep the frame budget under control; on small graphs the 140-node
        // threshold keeps the interactive feel of always-on labels.
        hideLabelsOnMove: ultraDenseGraph || visibleNodes.length > 140,
        // Disabling `renderLabels` at ultra-dense node counts cuts the
        // Sigma per-frame cost by 30-50% (Sigma's label collision pass
        // is the dominant hot path at 15k+ nodes) with no visual loss
        // because individual labels are unreadable at that density.
        renderLabels: !labelsDisabled,
        renderEdgeLabels: false,
        labelFont: 'Inter, system-ui, sans-serif',
        labelSize: 12,
        labelWeight: '500',
        labelColor: { color: '#94a3b8' },
        defaultNodeColor: '#78716c',
        defaultEdgeColor: edgeColor,
        defaultEdgeType,
        edgeProgramClasses: {
          curvedArrow: EdgeCurvedArrowProgram,
        },
        labelDensity,
        labelGridCellSize,
        labelRenderedSizeThreshold,
        autoCenter: true,
        autoRescale: true,
        zIndex: true,
        minCameraRatio: 0.01,
        maxCameraRatio: 50,
        zoomDuration: 50,
        zoomingRatio: 1.2,
        allowInvalidContainer: true,
      })

      sigmaInstance = sigma
      const camera = sigma.getCamera()
      lastTopologyRef.current = { nodes, edges }

      // Node dragging.
      //
      // Every `graph.setNodeAttribute` emits Graphology's
      // `nodeAttributesUpdated`, and Sigma's internal listener responds by
      // re-running `updateNode` across ALL nodes and — because x/y are
      // layout-impacting — reprocessing the spatial index (O(N), no
      // skipIndexation). A high-poll mouse fires `mousemovebody` many times
      // per frame, so the naive handler paid that O(N) cost dozens of times
      // per frame on a 25k-node graph → drag stutter. Two guards fix it:
      //   1. Coalesce x AND y into ONE `mergeNodeAttributes` call so a single
      //      pointer move emits one update event, not two.
      //   2. rAF-throttle: stash the latest target position and commit it at
      //      most once per frame, capping drag cost at the 60 fps budget no
      //      matter how fast the mouse reports.
      let draggedNode: string | null = null
      let pendingDragPos: { x: number; y: number } | null = null
      let dragFrame: number | null = null
      let draggedIncidentEdges: string[] = []
      const refreshDraggedNode = (includeIncidentEdges: boolean) => {
        if (!draggedNode) return
        const sigmaForDrag = sigmaRef.current
        if (!sigmaForDrag) return
        try {
          sigmaForDrag.refresh({
            partialGraph: {
              nodes: [draggedNode],
              edges: includeIncidentEdges ? draggedIncidentEdges : [],
            },
            skipIndexation: true,
          })
        } catch {
          sigmaForDrag.refresh({ schedule: true, skipIndexation: true })
        }
      }
      const refreshDenseIncidentEdgesThenRestore = (node: string, incidentEdges: string[]) => {
        const sigmaForDrag = sigmaRef.current
        restoreDenseIncidentEdges({
          node,
          incidentEdges,
          sigma,
          graph,
          isCurrent: isDenseEdgeRestoreCurrent.bind(null, {
            buildToken,
            sigmaForDrag,
            sigma,
            sigmaRef,
            graphRef,
            graph,
            node,
          }),
          restoreLayers: restoreDenseBaseEdgeLayers,
          frameRef: denseEdgeRestoreFrame,
        })
      }

      const cacheDenseDragOverlayTargets = (node: string) => {
        const targets: Array<{ x: number; y: number }> = []
        const hidden = hiddenIdsRef.current
        const neighbors = neighborIndexRef.current.get(node) ?? new Set<string>()
        for (const neighbor of neighbors) {
          if (targets.length >= DRAG_NEIGHBORHOOD_OVERLAY_EDGE_LIMIT) break
          if (hidden?.has(neighbor) || !graph.hasNode(neighbor)) continue
          const targetX = graph.getNodeAttribute(neighbor, 'x') as number | undefined
          const targetY = graph.getNodeAttribute(neighbor, 'y') as number | undefined
          if (targetX == null || targetY == null) continue
          targets.push(sigma.graphToViewport({ x: targetX, y: targetY }))
        }
        dragOverlayTargetsRef.current = targets
      }
      const moveDragPreview = (clientX: number, clientY: number) => {
        const preview = dragPreviewRef.current
        if (!preview) return
        preview.style.transform = `translate3d(${clientX + 12}px, ${clientY + 12}px, 0)`
      }
      const showDragPreview = (node: string, clientX: number, clientY: number) => {
        const preview = dragPreviewRef.current
        if (!preview) return
        preview.dataset.dragNodeId = node
        preview.hidden = false
        preview.style.visibility = 'visible'
        if (dragPreviewDotRef.current) {
          const dotColor =
            (graph.getNodeAttribute(node, 'color') as string | undefined) ??
            GRAPH_NODE_COLORS.entity
          if (typeof dotColor === 'string') {
            dragPreviewDotRef.current.style.backgroundColor = dotColor
          }
        }
        if (dragPreviewLabelRef.current) {
          dragPreviewLabelRef.current.textContent =
            labelByNodeId.get(node) ??
            (graph.getNodeAttribute(node, 'originalLabel') as string | undefined) ??
            node
        }
        moveDragPreview(clientX, clientY)
      }
      const mergeNodePositionWithoutSigmaListener = (
        node: string,
        position: { x: number; y: number },
      ) => {
        const saved = graph.listeners('nodeAttributesUpdated')
        graph.removeAllListeners('nodeAttributesUpdated')
        try {
          graph.mergeNodeAttributes(node, { x: position.x, y: position.y })
        } finally {
          for (const listener of saved) graph.on('nodeAttributesUpdated', listener)
        }
      }
      const flushDragPosition = () => {
        dragFrame = null
        // `buildToken.cancelled` flips when the effect is torn down (topology
        // / layout change, unmount); skip so a queued frame never mutates a
        // graph whose Sigma instance was already killed.
        if (buildToken.cancelled || !draggedNode || !pendingDragPos) return
        if (useDomOnlyInteractions) {
          // Dense graphs keep incident edges in the lightweight screen-space
          // overlay, but the node itself still has to move with the cursor.
          // Updating only one node per frame avoids the stale fixed dot without
          // paying the heavy incident-edge repaint path.
          mergeNodePositionWithoutSigmaListener(draggedNode, pendingDragPos)
          updateAllEdgesLayerNodePosition(draggedNode, pendingDragPos)
          refreshDraggedNode(false)
          drawNeighborhoodOverlayNow()
          pendingDragPos = null
        } else {
          // Small graphs can afford to mutate the graph and repaint the dragged
          // node + incident edges each frame; the Sigma listener stays suppressed
          // so x/y updates do not trigger a full spatial reindex mid-drag.
          mergeNodePositionWithoutSigmaListener(draggedNode, pendingDragPos)
          refreshDraggedNode(true)
          pendingDragPos = null
        }
      }

      const beginDenseDrag = (node: string) => {
        if (denseEdgeRestoreFrame.current != null) {
          cancelAnimationFrame(denseEdgeRestoreFrame.current)
          denseEdgeRestoreFrame.current = null
        }
        setDenseBaseEdgeLayersHidden(true)
        if (allEdgesLayerStateRef.current?.kind !== 'indexed') {
          setAllEdgesLayerHidden(true)
        }
        cacheDenseDragOverlayTargets(node)
        neighborhoodOverlayFocusRef.current = { nodeId: node, mode: 'drag' }
        scheduleNeighborhoodOverlayDraw()
        const x = graph.getNodeAttribute(node, 'x') as number | undefined
        const y = graph.getNodeAttribute(node, 'y') as number | undefined
        const viewport = x == null || y == null ? { x: 0, y: 0 } : sigma.graphToViewport({ x, y })
        dragOverlaySourceRef.current = viewport
        const rect = containerRef.current?.getBoundingClientRect()
        showDragPreview(node, viewport.x + (rect?.left ?? 0), viewport.y + (rect?.top ?? 0))
      }
      const beginDrag = (node: string) => {
        draggedNode = node
        draggedIncidentEdges = graph.edges(node)
        dragStateRef.current = { dragging: true, node }
        pendingHoverRef.current = null
        if (hoverTimerRef.current != null) {
          clearTimeout(hoverTimerRef.current)
          hoverTimerRef.current = null
        }
        setHoveredId(null)
        setTooltip(null)
        setTooltipPos(null)
        if (useDomOnlyInteractions) {
          beginDenseDrag(node)
        } else {
          graph.setNodeAttribute(node, 'highlighted', true)
        }
        camera.disable()
      }

      sigma.on('downNode', ({ node }) => beginDrag(node))

      sigma.getMouseCaptor().on('mousemovebody', (e: SigmaPointerCaptorEvent) => {
        if (!draggedNode) return
        pendingDragPos = sigma.viewportToGraph(e)
        // This captor only ever emits with a real `MouseEvent` (see the
        // `SigmaPointerCaptorEvent.original` comment above); narrow via the
        // mouse-only `clientX` member (absent on `TouchEvent`) to read
        // `clientX`/`clientY` below.
        if (useDomOnlyInteractions && 'clientX' in e.original) {
          moveDragPreview(e.original.clientX, e.original.clientY)
          const rect = containerRef.current?.getBoundingClientRect()
          dragOverlaySourceRef.current = {
            x: e.original.clientX - (rect?.left ?? 0),
            y: e.original.clientY - (rect?.top ?? 0),
          }
        }
        dragFrame ??= requestAnimationFrame(flushDragPosition)
        e.preventSigmaDefault()
        e.original.preventDefault()
        e.original.stopPropagation()
      })

      const finalizeDrag = () => {
        if (!draggedNode) return
        const releasedNode = draggedNode
        const releasedIncidentEdges = draggedIncidentEdges
        if (dragFrame != null) {
          cancelAnimationFrame(dragFrame)
          dragFrame = null
        }
        if (pendingDragPos) {
          if (useDomOnlyInteractions) {
            mergeNodePositionWithoutSigmaListener(draggedNode, pendingDragPos)
            updateAllEdgesLayerNodePosition(draggedNode, pendingDragPos)
          } else {
            graph.mergeNodeAttributes(draggedNode, { x: pendingDragPos.x, y: pendingDragPos.y })
          }
          pendingDragPos = null
        }
        if (useDomOnlyInteractions) {
          refreshDenseIncidentEdgesThenRestore(releasedNode, releasedIncidentEdges)
          if (allEdgesLayerStateRef.current?.kind === 'indexed') {
            scheduleAllEdgesLayerDraw()
          } else {
            rebuildAllEdgesLayer(graph, visibleEdges, useAllEdgesLayer)
          }
          setAllEdgesLayerHidden(false)
          hideDragPreview()
          dragOverlayTargetsRef.current = null
          dragOverlaySourceRef.current = null
          const selectedNode = selectedIdRef.current
          neighborhoodOverlayFocusRef.current =
            selectedNode && graph.hasNode(selectedNode)
              ? { nodeId: selectedNode, mode: 'selected' }
              : null
          scheduleNeighborhoodOverlayDraw()
        } else {
          graph.removeNodeAttribute(draggedNode, 'highlighted')
        }
        camera.enable()
        draggedNode = null
        draggedIncidentEdges = []
        dragStateRef.current = { dragging: false, node: null }
      }

      sigma.getMouseCaptor().on('mouseup', finalizeDrag)

      // Pointer cursor on node hover. Hover state is rAF-throttled via
      // `scheduleHoverUpdate` so cursor sweeps through dense graphs do not
      // queue dozens of React rerenders + sigma refreshes per second.
      //
      // We also drive a floating DOM tooltip with the node label + its
      // neighbor names. Tooltip is pure CSS/DOM — completely outside the
      // Sigma render path — so it works on dense graphs without paying the
      // ~120 ms `sigma.refresh()` cost per hover transition.
      // Only arm the dwell timer + set the cursor here. Building the tooltip
      // (neighbor labels + React setState) is deferred to the effect keyed on
      // the DWELL-COMMITTED `hoveredId` below, so sweeping the cursor fast
      // across a dense cluster never queues a tooltip re-render per node —
      // the tooltip only materialises once the cursor actually rests.
      sigma.on('enterNode', ({ node }) => {
        if (dragStateRef.current.dragging) return
        scheduleHoverUpdate(node)
        if (containerRef.current) containerRef.current.style.cursor = 'pointer'
      })
      sigma.on('leaveNode', () => {
        if (dragStateRef.current.dragging) return
        scheduleHoverUpdate(null)
        if (containerRef.current) containerRef.current.style.cursor = 'default'
        setTooltip(null)
        setTooltipPos(null)
      })
      // Zoom-driven LOD: only tier crossings re-thin the graph and the work is
      // deferred until camera movement settles, keeping zoom frames smooth.
      lodScheduler = createLodScheduler({
        visibleNodeCount: visibleNodes.length,
        sigma,
        getThreshold: () => lodThresholdRef.current,
        setThreshold: setLodThreshold,
        tiers: lodTiers,
        isCurrent: () => sigmaRef.current === sigma,
      })
      const applyLodForRatio = lodScheduler.apply
      const scheduleLodApply = lodScheduler.schedule
      // Reposition the card on camera move so it stays glued to the node
      // when the user pans/zooms with the hover still active.
      camera.on('updated', () => {
        scheduleLodApply()
        onCameraMoveEdges()
        if (useDomOnlyInteractions && neighborhoodOverlayFocusRef.current) {
          scheduleNeighborhoodOverlayDraw()
        }
        const current = tooltipRef.current
        if (!current) return
        const activeNodeId = current.dataset.nodeId
        if (!activeNodeId || !graph.hasNode(activeNodeId)) return
        const x = graph.getNodeAttribute(activeNodeId, 'x') as number | undefined
        const y = graph.getNodeAttribute(activeNodeId, 'y') as number | undefined
        if (x == null || y == null) return
        const viewport = sigma.graphToViewport({ x, y })
        const containerRect = containerRef.current?.getBoundingClientRect()
        current.style.left = `${viewport.x + (containerRect?.left ?? 0) + 12}px`
        current.style.top = `${viewport.y + (containerRect?.top ?? 0) + 12}px`
      })
      sigma.on('afterRender', () => {
        onCameraMoveEdges()
        if (useDomOnlyInteractions && neighborhoodOverlayFocusRef.current) {
          scheduleNeighborhoodOverlayDraw()
        }
      })

      sigma.on('clickNode', ({ node }) => {
        if (!dragStateRef.current.dragging) {
          if (useDomOnlyInteractions) {
            neighborhoodOverlayFocusRef.current = { nodeId: node, mode: 'selected' }
            scheduleNeighborhoodOverlayDraw()
          }
          onSelectRef.current(node)
        }
      })
      sigma.on('clickStage', () => {
        setHoveredId(null)
        if (!dragStateRef.current.dragging) {
          if (useDomOnlyInteractions) {
            neighborhoodOverlayFocusRef.current = null
            scheduleNeighborhoodOverlayDraw()
          }
          onSelectRef.current(null)
        }
      })

      sigmaRef.current = sigma
      // Fresh Sigma instance owns a fresh program index. Any node/edge ids
      // tracked from a previous topology are now invalid and must never be
      // handed to `partialGraph` + `skipIndexation` (Sigma throws
      // "can't be repaint" for an id it has no program slot for). Clear the
      // affected-set trackers so the next reducer run starts from empty.
      affectedNodeIdsRef.current = new Set()
      affectedEdgeIdsRef.current = new Set()
      hiddenEdgeIdsRef.current = null
      setGraphInstanceVersion((version) => version + 1)
      rebuildAllEdgesLayer(graph, visibleEdges, useAllEdgesLayer)
      restoreDenseBaseEdgeLayers()
      onFitViewReadyRef.current?.(() => {
        resetSigmaCamera(sigmaRef.current ?? sigma, 280)
      })
      if (reusedCameraState) {
        sigma.getCamera().setState(reusedCameraState)
      } else {
        requestAnimationFrame(() => resetSigmaCamera(sigma, 180))
      }
      // Apply the initial LOD once the fit animation + first process() have
      // settled. Firing it mid-initial-render loses the hide to the in-flight
      // pass (the reducer runs but the fresh instance hasn't committed its
      // first process yet), which is why the camera-driven path alone left the
      // overview un-thinned.
      if (visibleNodes.length > LOD_NODE_THRESHOLD) {
        window.setTimeout(() => {
          if (sigmaRef.current !== sigma) return
          applyLodForRatio()
          // Re-apply the (now-installed) LOD reducer after the initial render
          // has committed, so the hide sticks even though the first pass
          // swallowed it against the freshly-built instance.
          sigma.refresh({})
        }, 700)
      }
    })

    return () => {
      // Abort any in-flight worker layout before the cleanup runs so
      // the `.then` body short-circuits before it ever touches Sigma.
      buildToken.cancelled = true
      lodScheduler?.clear()
      stopLayoutAnimation()
      // Invalidate any in-flight layout-switch worker result (it guards on
      // this token) and clear the recomputing affordance so a topology /
      // library change never leaves the spinner stuck on.
      layoutSwitchTokenRef.current += 1
      forceClearAllEdgesLayerSuspension()
      setLayoutRecomputing(false)
      if (hoverTimerRef.current != null) {
        clearTimeout(hoverTimerRef.current)
        hoverTimerRef.current = null
      }
      pendingHoverRef.current = null
      setHoveredId(null)
      setTooltip(null)
      neighborhoodOverlayFocusRef.current = null
      dragOverlayTargetsRef.current = null
      dragOverlaySourceRef.current = null
      clearNeighborhoodOverlay()
      clearAllEdgesLayer()
      hideDragPreview()
      if (denseEdgeRestoreFrame.current != null) {
        cancelAnimationFrame(denseEdgeRestoreFrame.current)
        denseEdgeRestoreFrame.current = null
      }
      setDenseBaseEdgeLayersHidden(false)
      if (sigmaInstance) {
        lastCameraStateRef.current = sigmaInstance.getCamera().getState()
        sigmaInstance.kill()
      }
      renderedEdgeIdsRef.current = []
      sigmaRef.current = null
    }
    // `layout` is intentionally NOT a dependency. This effect tears down and
    // rebuilds the entire Graphology graph + Sigma instance, which is only
    // warranted when the TOPOLOGY changes. A layout/mode switch must NOT
    // rebuild — it is handled by the dedicated layout-switch effect below,
    // which just re-applies node positions to the existing instance. Including
    // `layout` here made every mode switch pay a full graph rebuild on top of
    // the position apply. The effect closure still reads the current `layout`
    // when it DOES run (React recreates the closure per render), so a topology
    // change still builds with the active layout.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    clearAllEdgesLayer,
    clearNeighborhoodOverlay,
    drawNeighborhoodOverlayNow,
    edges,
    forceClearAllEdgesLayerSuspension,
    labelByNodeId,
    neighborIndex,
    nodes,
    rebuildAllEdgesLayer,
    resolvedTheme,
    scheduleAllEdgesLayerDraw,
    scheduleNeighborhoodOverlayDraw,
    updateAllEdgesLayerNodePosition,
  ])

  useEffect(() => {
    const sigma = sigmaRef.current
    const graph = graphRef.current
    if (!sigma || !graph || nodes.length === 0) return
    if (layoutRef.current === layout) return

    stopLayoutAnimation()
    const previousLayout = layoutRef.current
    layoutRef.current = layout

    const reduceMotion =
      typeof window !== 'undefined' &&
      typeof window.matchMedia === 'function' &&
      window.matchMedia('(prefers-reduced-motion: reduce)').matches

    const order = graph.order
    const useAllEdgesLayer =
      nodes.length >= ALL_EDGES_LAYER_NODE_THRESHOLD ||
      edges.length > ALL_EDGES_LAYER_EDGE_THRESHOLD

    const refreshRenderedEdgesChunked = async (token: number): Promise<boolean> => {
      const cachedRenderedEdges = renderedEdgeIdsRef.current
      const renderedEdges = cachedRenderedEdges.length > 0 ? cachedRenderedEdges : graph.edges()
      const edgeChunkSize = order >= 50000 ? 500 : 4000
      return refreshEdgeChunks({
        edgeIds: renderedEdges,
        chunkSize: edgeChunkSize,
        isCurrent: () =>
          layoutSwitchTokenRef.current === token &&
          graphRef.current === graph &&
          sigmaRef.current === sigma,
        refresh: (edgeChunk) => {
          sigma.refresh({
            partialGraph: { nodes: [], edges: edgeChunk },
            schedule: true,
            skipIndexation: true,
          })
        },
        waitForFrame: waitForAnimationFrame,
      })
    }

    const applyPositionsChunked = async (
      layoutNodes: Array<{ id: string }>,
      positions: ArrayLike<number>,
      token: number,
    ): Promise<boolean> => {
      const isCurrent = () =>
        layoutSwitchTokenRef.current === token &&
        graphRef.current === graph &&
        sigmaRef.current === sigma
      const applied = await applyLayoutPositionsChunked({
        layoutNodes,
        positions,
        chunkSize: order >= 50000 ? 500 : 5000,
        isCurrent,
        apply: (id, x, y) => {
          const savedNodeListeners = graph.listeners('nodeAttributesUpdated')
          graph.removeAllListeners('nodeAttributesUpdated')
          try {
            graph.mergeNodeAttributes(id, { x, y })
          } finally {
            for (const listener of savedNodeListeners) graph.on('nodeAttributesUpdated', listener)
          }
        },
        refresh: (nodeIds) => {
          sigma.refresh({
            partialGraph: { nodes: nodeIds, edges: [] },
            schedule: true,
            skipIndexation: true,
          })
        },
        waitForFrame: waitForAnimationFrame,
      })
      if (!applied) return false

      const hasActiveIndexedAllEdgesLayer =
        useAllEdgesLayer &&
        allEdgesLayerEnabledRef.current &&
        allEdgesLayerStateRef.current?.kind === 'indexed'
      if (!hasActiveIndexedAllEdgesLayer && !(await refreshRenderedEdgesChunked(token)))
        return false
      const indexedLayerState = await writeAllEdgesLayerPositionsFromLayoutChunked(
        layoutNodes,
        positions,
        useAllEdgesLayer,
        isCurrent,
      )
      if (indexedLayerState) {
        if (!(await uploadAllEdgesLayerPositionTextureChunked(indexedLayerState, isCurrent)))
          return false
      } else {
        rebuildAllEdgesLayer(graph, edges, useAllEdgesLayer)
      }
      scheduleNeighborhoodOverlayDraw()
      if (order < 50000) resetSigmaCamera(sigma, 200)
      await waitForAnimationFrame()
      if (!isCurrent()) return false
      await waitForAnimationFrame()
      return isCurrent()
    }

    // ULTRA-DENSE (≥ INSTANT_LAYOUT_NODE_THRESHOLD, e.g. 100k nodes) — OR any
    // ITERATIVE (force) layout at any size. Per-frame interpolation is
    // pointless at scale (the eye cannot track 100k dots drifting) AND
    // computing the target on the main thread is a ~1 s stall — the clone
    // alone was ~770 ms at 100k, and FA2's hundreds of passes are worse. Route
    // the whole computation to the worker using the cached slim payload (no
    // clone, no second main-thread Graphology build), show a "recomputing"
    // affordance, then apply the result in frame-sized chunks so Firefox never
    // gets one large repaint/upload burst. Force MUST take this path: the sync
    // branches below only run `applyGraphLayout`, which for 'force' is just the
    // geometric seed with no simulation.
    const payload = workerPayloadRef.current
    if ((order >= INSTANT_LAYOUT_NODE_THRESHOLD || isIterativeLayout(layout)) && payload) {
      const token = layoutSwitchTokenRef.current + 1
      layoutSwitchTokenRef.current = token
      if (useAllEdgesLayer) {
        suspendAllEdgesLayerDraws(token)
      }
      setLayoutRecomputing(true)
      void (async () => {
        let applied = false
        try {
          const result = await computeGraphLayoutOffThread({
            nodes: payload.nodes,
            edges: payload.edges,
            layout,
            cacheKey: payload,
          })
          // Discard if a newer switch superseded this one, or the graph
          // was torn down (topology change / unmount) while computing.
          if (layoutSwitchTokenRef.current !== token) return
          if (graphRef.current !== graph || sigmaRef.current !== sigma) return
          applied = await applyPositionsChunked(payload.nodes, result.positions, token)
          if (applied && useAllEdgesLayer) {
            resumeAllEdgesLayerDraws(token, true)
          }
        } catch (error) {
          if (layoutSwitchTokenRef.current !== token) return
          if (graphRef.current !== graph || sigmaRef.current !== sigma) return
          // Keep the previous layout rather than reintroducing the dense
          // synchronous layout stall on the UI thread.
          console.warn('[graph] layout switch failed, keeping previous layout', error)
          layoutRef.current = previousLayout
        } finally {
          if (layoutSwitchTokenRef.current === token) {
            if (useAllEdgesLayer && !applied) {
              resumeAllEdgesLayerDraws(token, true)
            }
            setLayoutRecomputing(false)
          }
        }
      })()
      return () => {
        stopLayoutAnimation()
      }
    }

    // MID-DENSITY instant path (no animation but cheap to compute on the
    // main thread). Compute the target layout directly on the live graph
    // — there is no clone. The brief sync layout pass is well under a
    // frame at these node counts, and we never interpolate.
    //
    // `useAllEdgesLayer` also routes here: when the custom WebGL edge
    // overlay is active (>70k edges, or >=15k nodes) the per-frame animated
    // path below cannot keep the overlay in sync — it repaints edges from a
    // stale position texture while the nodes drift, so the edges visibly
    // detach for the whole ~280 ms transition and snap at the end. The
    // instant path re-syncs the overlay in the SAME tick as the node
    // refresh (`syncOrRebuildAllEdgesLayerPositions` below), so edges stay
    // attached. Losing the eased drift on such graphs is negligible; the
    // eased animation only ever looked right when Sigma drew its own edges.
    if (reduceMotion || order === 0 || order >= INSTANT_LAYOUT_NODE_THRESHOLD || useAllEdgesLayer) {
      applyGraphLayout(graph, layout)
      sigma.refresh({ skipIndexation: true })
      syncOrRebuildAllEdgesLayerPositions(graph, edges, useAllEdgesLayer)
      scheduleNeighborhoodOverlayDraw()
      resetSigmaCamera(sigma, 140)
      return () => {
        stopLayoutAnimation()
      }
    }

    // SMALL graph: keep the beautiful eased per-frame transition. Compute
    // the target positions WITHOUT cloning — snapshot the current x/y,
    // apply the target layout to the live graph to read the destination,
    // then restore the from-positions and animate between them. At these
    // node counts the double layout pass is negligible and the smooth
    // drift is the whole point of the small-graph experience.
    const transitionNodes: Array<{
      node: string
      fromX: number
      fromY: number
      toX: number
      toY: number
    }> = []
    graph.forEachNode((node, attr) => {
      transitionNodes.push({
        node,
        fromX: (attr.x as number) ?? 0,
        fromY: (attr.y as number) ?? 0,
        toX: 0,
        toY: 0,
      })
    })
    applyGraphLayout(graph, layout)
    for (const transition of transitionNodes) {
      transition.toX = (graph.getNodeAttribute(transition.node, 'x') as number) ?? 0
      transition.toY = (graph.getNodeAttribute(transition.node, 'y') as number) ?? 0
      // Restore the starting position so the first animated frame begins
      // from where the node currently is, not from its destination.
      graph.setNodeAttribute(transition.node, 'x', transition.fromX)
      graph.setNodeAttribute(transition.node, 'y', transition.fromY)
    }

    const animationToken = layoutAnimationTokenRef.current + 1
    layoutAnimationTokenRef.current = animationToken
    const startedAt = performance.now()

    const renderFrame = (now: number) => {
      if (layoutAnimationTokenRef.current !== animationToken) return

      const progress = Math.min(1, (now - startedAt) / LAYOUT_ANIMATION_DURATION_MS)
      const eased = 1 - Math.pow(1 - progress, 3)

      for (const transition of transitionNodes) {
        graph.setNodeAttribute(
          transition.node,
          'x',
          transition.fromX + (transition.toX - transition.fromX) * eased,
        )
        graph.setNodeAttribute(
          transition.node,
          'y',
          transition.fromY + (transition.toY - transition.fromY) * eased,
        )
      }

      sigma.refresh({ skipIndexation: true })

      if (progress < 1) {
        layoutAnimationFrameRef.current = requestAnimationFrame(renderFrame)
      } else {
        layoutAnimationFrameRef.current = null
        syncOrRebuildAllEdgesLayerPositions(graph, edges, useAllEdgesLayer)
        resetSigmaCamera(sigma, 180)
        scheduleNeighborhoodOverlayDraw()
      }
    }

    layoutAnimationFrameRef.current = requestAnimationFrame(renderFrame)

    return () => {
      stopLayoutAnimation()
    }
  }, [
    edges,
    layout,
    nodes,
    rebuildAllEdgesLayer,
    resumeAllEdgesLayerDraws,
    scheduleNeighborhoodOverlayDraw,
    syncOrRebuildAllEdgesLayerPositions,
    suspendAllEdgesLayerDraws,
    uploadAllEdgesLayerPositionTextureChunked,
    writeAllEdgesLayerPositionsFromLayoutChunked,
  ])

  // Recompute hidden edge ids whenever `hiddenIds` (or the underlying
  // topology) changes — O(M) once per change instead of O(M) once per
  // hover. The ref is read by the reducer effect below without
  // triggering its own re-run, so hover transitions do not pay the
  // scan cost.
  useEffect(() => {
    const graph = graphRef.current
    if (!graph) {
      hiddenEdgeIdsRef.current = null
      return
    }
    const useAllEdgesLayer =
      nodes.length >= ALL_EDGES_LAYER_NODE_THRESHOLD ||
      edges.length > ALL_EDGES_LAYER_EDGE_THRESHOLD
    if (!effectiveHiddenIds || effectiveHiddenIds.size === 0) {
      hiddenEdgeIdsRef.current = null
      rebuildAllEdgesLayer(graph, edges, useAllEdgesLayer)
      return
    }
    const hidden = new Set<string>()
    graph.forEachEdge((edge, _attrs, source, target) => {
      if (effectiveHiddenIds.has(source) || effectiveHiddenIds.has(target)) {
        hidden.add(edge)
      }
    })
    hiddenEdgeIdsRef.current = hidden
    rebuildAllEdgesLayer(graph, edges, useAllEdgesLayer)
  }, [effectiveHiddenIds, nodes, edges, graphInstanceVersion, rebuildAllEdgesLayer])

  useEffect(() => {
    const sigma = sigmaRef.current
    const graph = graphRef.current
    if (!sigma || !graph) return

    // Filters are applied through Sigma's reducer pipeline — never by
    // rebuilding the Graphology instance. On a 100k-node / 100k-edge graph
    // a teardown + layout + re-init burns multiple seconds per keystroke;
    // the reducer path runs in a few milliseconds because Graphology state
    // is untouched.
    //
    // Hidden-edge set is owned by `hiddenEdgeIdsRef` (built by the
    // dedicated effect above). Reading a ref here keeps the reducer
    // effect off the hidden-edge dependency graph — hover transitions
    // would otherwise rerun the O(M) scan even when `hiddenIds` is
    // unchanged.
    const hiddenNodeSet =
      effectiveHiddenIds && effectiveHiddenIds.size > 0 ? effectiveHiddenIds : null
    const filterHiddenEdgeIds = hiddenEdgeIdsRef.current ?? EMPTY_EDGE_SET
    const lodHiddenEdgeIds = showDenseEdges ? EMPTY_EDGE_SET : edgeLodExtraEdgeIdsRef.current
    const hasHiddenEdges = filterHiddenEdgeIds.size > 0 || lodHiddenEdgeIds.size > 0
    const isEdgeHidden = (edge: string): boolean =>
      filterHiddenEdgeIds.has(edge) || lodHiddenEdgeIds.has(edge)
    const useDomOnlyInteractions = nodes.length >= DOM_ONLY_INTERACTION_NODE_THRESHOLD

    // Accumulate the ids this run visually changes. Only these — plus the
    // ones the PREVIOUS run changed (so we can restore them to base style)
    // — get re-reduced + repainted via `partialGraph`. Everything else
    // keeps its already-rendered attributes untouched, turning the former
    // O(N) refresh into an O(affected) one.
    const nextAffectedNodes = new Set<string>()
    const nextAffectedEdges = new Set<string>()
    // `skipIndexation: true` reuses each element's existing program slot
    // and skips Sigma's whole-graph reprocess — correct for pure visual
    // attribute changes (size / color / label / hidden / highlighted).
    // EVERY branch keeps this true, so there is never a per-interaction
    // GPU reindex.
    //
    // Sigma treats `zIndex` as a layout-impacting field: a reducer-written
    // `zIndex` only re-sorts draw order inside `process()`, which runs only
    // when `skipIndexation` is false (grounded in sigma@3.0.3
    // `refresh()` → `needToProcess`/`zIndexOrdering`). The selection branch
    // used to rewrite `zIndex` graph-wide to layer the focused node +
    // neighbors above the faded rest, which forced a full O(N) reindex on
    // every click AND every deselect — the click freeze on a 100k-node /
    // ~300k-edge graph. We removed those `zIndex` writes: the selected node
    // already draws on the dedicated `highlightedNodes` top pass (via
    // `highlighted: true`), independent of the z-sort, and the size contrast
    // (focus 9 / neighbor 7 vs faded 2) plus near-invisible white/size-0.05
    // faded edges preserve the spotlight without a reindex. Now selection
    // and deselection are reducer-cache-only — no GPU reprocess, no freeze.
    const skipIndexation = true

    // Three distinct interaction modes (all composed with the filter):
    //
    // CLICK (selectedId set): full focus mode. Selected node + its edges
    // pop out, every other node fades to gray, every other edge fades.
    //
    // HOVER (hoveredId set, no selection): soft hint only. Highlight the
    // hovered node and its neighbors with a label + slight size bump.
    //
    // IDLE: either a pure filter pass (when hiddenIds is non-empty) or
    // null reducers so the graph renders at its base style.
    //
    // The hidden check must run FIRST in every branch so filters always
    // win over selection/hover highlighting.
    const reducerMode = configureInteractionReducers({
      sigma,
      graph,
      selectedId,
      hoveredId,
      neighborIndex,
      hiddenNodeSet,
      filterHiddenEdgeIds,
      lodHiddenEdgeIds,
      hasHiddenEdges,
      isEdgeHidden,
      useDomOnlyInteractions,
      nextAffectedNodes,
      nextAffectedEdges,
    })
    if (reducerMode === 'dom-idle') {
      affectedNodeIdsRef.current = new Set()
      affectedEdgeIdsRef.current = new Set()
      return
    }

    // Partial refresh over {previously affected} ∪ {now affected}: Sigma
    // re-applies the reducers and repaints ONLY these ids, reusing every
    // other element's already-computed render data. `skipIndexation` is
    // true for EVERY branch now (no branch rewrites x/y or zIndex — the
    // selection branch dropped its zIndex writes in favor of the
    // `highlightedNodes` top pass), so Sigma always skips the whole-graph
    // `process()`/`zIndexOrdering` reprocess. Hover/filter/idle stay
    // O(affected); selection touches the full node/edge set but only as
    // cheap cache writes, never a GPU reindex — eliminating both the
    // ~120 ms hover-stop block and the click/deselect freeze on large
    // graphs. (This mirrors Sigma's own `eachNodeAttributesUpdated`
    // internal path, which likewise calls
    // `refresh({ partialGraph, skipIndexation })`.) The id list is filtered
    // through `graph.hasNode`/`hasEdge` so a stale id can never reach
    // `skipIndexation`'s repaint, which throws for an unindexed element.
    const refreshNodes = collectValidIds(affectedNodeIdsRef.current, nextAffectedNodes, (id) =>
      graph.hasNode(id),
    )
    const refreshEdges = collectValidIds(affectedEdgeIdsRef.current, nextAffectedEdges, (id) =>
      graph.hasEdge(id),
    )

    if (refreshNodes.length === 0 && refreshEdges.length > 2000) {
      let cancelled = false
      const chunkSize = 100
      void (async () => {
        for (let offset = 0; offset < refreshEdges.length; offset += chunkSize) {
          if (cancelled || graphRef.current !== graph || sigmaRef.current !== sigma) return
          const edgeChunk = refreshEdges.slice(offset, offset + chunkSize)
          try {
            sigma.refresh({
              partialGraph: { nodes: [], edges: edgeChunk },
              schedule: true,
              skipIndexation,
            })
          } catch {
            sigma.refresh({ schedule: true })
            return
          }
          await waitForAnimationFrame()
        }
      })()

      affectedNodeIdsRef.current = nextAffectedNodes
      affectedEdgeIdsRef.current = nextAffectedEdges
      return () => {
        cancelled = true
      }
    }

    // A LARGE affected-node set means a filter / LOD change, not a small
    // hover/selection tweak. Toggling `hidden` on thousands of nodes only
    // takes effect after Sigma RE-INDEXES (rebuilds its program buffers) —
    // `skipIndexation` would repaint the existing slots and leave the hidden
    // nodes on screen. So pay one full `refresh()` here; it is O(N) but only
    // fires on a filter edit or an LOD zoom-tier crossing, never per frame.
    if (refreshNodes.length > 4000) {
      affectedNodeIdsRef.current = nextAffectedNodes
      affectedEdgeIdsRef.current = nextAffectedEdges
      sigma.refresh({})
      return
    }

    // `skipIndexation: true` repaints each id into its EXISTING program slot
    // (`edgeProgramIndex[id]` / `nodeProgramIndex[id]`) and throws
    // "can't be repaint" if that slot is missing — which happens when this
    // reducer effect runs against a freshly-rebuilt Sigma instance that has
    // not completed its first `process()` yet (the build effect and this
    // effect can interleave on a data/selection change). An uncaught throw
    // here propagates out of Sigma's render and loses the WebGL context,
    // blanking the canvas. So fall back to a full `sigma.refresh()` (which
    // re-indexes the whole graph and can never throw). Steady-state
    // interactions keep the fast partial path; only the rare pre-process
    // window pays one full refresh.
    try {
      sigma.refresh({
        partialGraph: { nodes: refreshNodes, edges: refreshEdges },
        skipIndexation,
      })
    } catch {
      sigma.refresh()
    }

    affectedNodeIdsRef.current = nextAffectedNodes
    affectedEdgeIdsRef.current = nextAffectedEdges
  }, [
    hoveredId,
    neighborIndex,
    selectedId,
    effectiveHiddenIds,
    showDenseEdges,
    graphInstanceVersion,
    nodes.length,
  ])

  return (
    <div className="relative isolate h-full w-full">
      <div className="pointer-events-none absolute inset-0 z-0 h-full w-full">
        <canvas ref={allEdgesCanvasRef} className="h-full w-full" />
      </div>
      <div
        ref={containerRef}
        className="relative z-10 h-full w-full"
        style={{ minHeight: '400px' }}
      />
      <div className="pointer-events-none absolute inset-0 z-20 h-full w-full">
        <canvas ref={neighborhoodCanvasRef} className="h-full w-full" />
      </div>
      {layoutRecomputing && (
        <div
          role="status"
          aria-live="polite"
          className="pointer-events-none absolute inset-0 z-40 flex items-center justify-center"
        >
          <div className="inline-flex items-center gap-2.5 rounded-full border border-border/70 bg-popover/95 px-4 py-2 text-sm font-medium text-popover-foreground shadow-elevated backdrop-blur-md">
            <Loader2 className="h-4 w-4 animate-spin text-primary" />
            {t('graph.recomputingLayout')}
          </div>
        </div>
      )}
      {tooltip && tooltipPos && (
        <div
          ref={tooltipRef}
          data-node-id={tooltip.nodeId}
          className="fixed pointer-events-none z-50 max-w-xs rounded-md border border-border bg-popover/95 px-3 py-2 text-xs text-popover-foreground shadow-lg backdrop-blur-sm"
          style={{ left: tooltipPos.x + 12, top: tooltipPos.y + 12 }}
        >
          <div className="font-semibold text-sm leading-tight mb-1 truncate">{tooltip.label}</div>
          <div className="text-muted-foreground text-2xs mb-1">
            {t('graph.edgeCount', { count: tooltip.neighborCount })}
          </div>
          {tooltip.neighborLabels.length > 0 && (
            <ul className="space-y-0.5 list-disc list-inside text-2xs text-muted-foreground">
              {tooltip.neighborLabels.map((label, index) => {
                const occurrence = tooltip.neighborLabels
                  .slice(0, index)
                  .filter((item) => item === label).length
                return (
                  <li key={`${label}-${occurrence}`} className="truncate">
                    {label}
                  </li>
                )
              })}
              {tooltip.neighborCount > tooltip.neighborLabels.length && (
                <li className="text-muted-foreground/70">
                  {t('common.moreCount', {
                    count: tooltip.neighborCount - tooltip.neighborLabels.length,
                  })}
                </li>
              )}
            </ul>
          )}
        </div>
      )}
      <div
        ref={dragPreviewRef}
        className="pointer-events-none fixed left-0 top-0 z-50 flex max-w-xs items-center gap-2 rounded-full border border-border/70 bg-popover/95 px-2.5 py-1.5 text-xs font-medium text-popover-foreground shadow-lg backdrop-blur-sm will-change-transform"
      >
        <span ref={dragPreviewDotRef} className="h-2.5 w-2.5 shrink-0 rounded-full" />
        <span ref={dragPreviewLabelRef} className="truncate" />
      </div>
    </div>
  )
}

export default memo(SigmaGraph)
