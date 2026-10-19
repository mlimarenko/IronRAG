/**
 * Marker size as a function of camera zoom.
 *
 * The fitted view intentionally draws 1..2.6 px dots for a 140k-node graph
 * (`buildNodeSizer`), which reads as a density map. Keeping that size while
 * zooming multiplies the distance between dots and turns a zoomed-in view
 * into specks far apart (IRG-121), so markers grow with the square root of
 * the zoom-in factor and stop at a readable cap.
 *
 * Two invariants:
 *   - `k <= 1` (fitted or zoomed out) scales by exactly 1, so the fitted
 *     frame stays pixel-identical to the pre-IRG-121 renderer;
 *   - a marker whose fitted radius is at most `NODE_RADIUS_CAP_PX` never grows
 *     past it; the cap stays below the active-emphasis floor (9.0) in the node
 *     vertex shader, so on a dense graph a hovered/selected node is strictly
 *     larger than any plain dot;
 *   - a fitted radius already above the cap (small graphs, ≤500 nodes) is
 *     never shrunk.
 *
 * `scaledNodeRadius` is the one law: the node vertex shader mirrors it with
 * the same cap interpolated from `NODE_RADIUS_CAP_PX`, and picking and label
 * offsets call it directly.
 */
export const NODE_SIZE_SCALE_CAP = 3
export const NODE_RADIUS_CAP_PX = 8

/** Zoom-in factor relative to the fitted view: >1 while zoomed in. */
export function zoomInFactor(fitRatio: number, ratio: number): number {
  return fitRatio / Math.max(1e-9, ratio)
}

/** Multiplier applied to every base radius at this zoom-in factor. */
export function nodeSizeScale(zoomInFactor: number): number {
  return Math.min(NODE_SIZE_SCALE_CAP, Math.max(1, Math.sqrt(zoomInFactor)))
}

/** On-screen radius in CSS px: base radius grown by `scale`, capped. */
export function scaledNodeRadius(baseRadius: number, scale: number): number {
  return Math.max(baseRadius, Math.min(baseRadius * scale, NODE_RADIUS_CAP_PX))
}
