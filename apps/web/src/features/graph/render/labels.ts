import { buildGraphCanvasLabel, buildGraphFocusLabel } from '../model/config'
import { queryViewportNodes, type SpatialGrid } from './spatialGrid'
import { screenToWorld, type CameraState, type Viewport } from './camera'
import { zoomInFactor } from './nodeScale'

/**
 * Zoom-driven label selection: labels live on a 2D overlay canvas (no DOM
 * nodes), and only the most-connected nodes currently inside the viewport
 * get one. The budget grows as the user zooms in, so a fully zoomed-out
 * 230k-node view shows a handful of anchor labels while a street-level view
 * labels everything in sight.
 */
export interface LabelCandidate {
  index: number
  text: string
}

/** How many labels fit a viewport at the given zoom-in factor. */
export function labelBudget(zoomInFactor: number): number {
  if (zoomInFactor >= 24) return 120
  if (zoomInFactor >= 10) return 70
  if (zoomInFactor >= 4) return 36
  if (zoomInFactor >= 1.8) return 20
  return 10
}

/** Neighbor labels reserved while a node is hovered or selected. */
export const FOCUS_NEIGHBOR_LABEL_CAP = 12

export function selectViewportLabels({
  grid,
  positions,
  degrees,
  labels,
  camera,
  viewport,
  fitRatio,
  focusIndices,
  neighborIndices,
  isVisible,
}: {
  grid: SpatialGrid
  positions: Float32Array
  degrees: Uint32Array
  labels: readonly string[]
  camera: CameraState
  viewport: Viewport
  /** Camera ratio of the fitted whole-graph view (zoom reference). */
  fitRatio: number
  /** Selected then hovered nodes: always labeled, with the long label form. */
  focusIndices?: readonly number[]
  /** Direct neighbors of the focus nodes; pinned after them, before degree rank. */
  neighborIndices?: readonly number[]
  isVisible: (index: number) => boolean
}): LabelCandidate[] {
  const factor = zoomInFactor(fitRatio, camera.ratio)
  const budget = labelBudget(factor)
  const topLeft = screenToWorld(camera, viewport, 0, 0)
  const bottomRight = screenToWorld(camera, viewport, viewport.width, viewport.height)

  // Scan a bounded number of viewport nodes, then keep the most connected.
  const candidates = queryViewportNodes(
    grid,
    positions,
    { minX: topLeft.x, minY: topLeft.y, maxX: bottomRight.x, maxY: bottomRight.y },
    4000,
    isVisible,
  )
  candidates.sort((left, right) => degrees[right]! - degrees[left]!)

  const nodeCountHint = grid.cellNodes.length
  const selected: LabelCandidate[] = []
  const used = new Set<number>()
  const push = (index: number, text: string) => {
    if (selected.length >= budget || used.has(index) || !isVisible(index)) return
    used.add(index)
    selected.push({ index, text })
  }

  if (focusIndices) {
    for (const index of focusIndices) {
      if (index >= 0) push(index, buildGraphFocusLabel(labels[index] ?? ''))
    }
  }
  if (neighborIndices) {
    const neighborLimit = Math.min(neighborIndices.length, FOCUS_NEIGHBOR_LABEL_CAP)
    for (let cursor = 0; cursor < neighborLimit; cursor += 1) {
      const index = neighborIndices[cursor]!
      push(index, buildGraphCanvasLabel(labels[index] ?? '', nodeCountHint))
    }
  }
  for (const index of candidates) {
    push(index, buildGraphCanvasLabel(labels[index] ?? '', nodeCountHint))
  }
  return selected
}
