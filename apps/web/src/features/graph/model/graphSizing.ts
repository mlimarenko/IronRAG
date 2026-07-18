import type { GraphNode } from '@/shared/types'

/**
 * Pixel-radius band per visible-node-count tier. Larger graphs use a smaller
 * band so a dense graph never paints as one solid colour block — a MEASURED
 * constraint: a fixed 3..13 band produced overlapping discs once the dataset
 * crossed ~5000 nodes. Kept verbatim from the original density clamp; only the
 * metric that feeds the band changed (see `buildNodeSizer`).
 *   * <=500 nodes:      3..13 px
 *   * 500..5000 nodes:  2..7 px
 *   * 5000..15000:      1.4..4 px
 *   * >15000:           1..2.6 px (treat as a density visualization)
 */
interface NodeSizeTier {
  min: number
  max: number
}

function resolveNodeSizeTier(nodeCount: number): NodeSizeTier {
  if (nodeCount > 15000) return { min: 1, max: 2.6 }
  if (nodeCount > 5000) return { min: 1.4, max: 4 }
  if (nodeCount > 500) return { min: 2, max: 7 }
  return { min: 3, max: 13 }
}

/**
 * Node radius encodes graph CONNECTIVITY — `node.edgeCount` is the node's
 * degree (distinct neighbours), the same number the tooltip and inspector
 * show. The degree distribution is heavy-tailed (most nodes have 0-1
 * neighbours), so mapping the raw metric through any fixed curve collapses
 * almost every node onto the tier floor — the "every node looks the same
 * size" bug. Instead each node maps to its PERCENTILE within the visible
 * distribution: the least-connected mass sits at the floor, hubs reach the
 * ceiling, and the connected middle spreads smoothly across the band. Because
 * percentile rank is scale-free, differentiation survives at every graph size,
 * while the pixel band itself (`resolveNodeSizeTier`) is untouched so the
 * anti-block-render guarantee still holds.
 *
 * Returns a reusable sizer so the O(n log n) distribution pass runs once per
 * build, not once per node.
 */
export function buildNodeSizer(nodes: GraphNode[]): (node: GraphNode) => number {
  const tier = resolveNodeSizeTier(nodes.length)
  const span = tier.max - tier.min
  const total = nodes.length
  if (total === 0 || span <= 0) return () => tier.min

  // `lessCountByMetric[m]` = how many nodes have a strictly smaller metric.
  // Ties share the lowest rank, so the isolated (degree-0) mass all lands on
  // the floor and only genuinely-more-connected nodes grow above it.
  const sorted = nodes.map((node) => node.edgeCount).sort((a, b) => a - b)
  const lessCountByMetric = new Map<number, number>()
  for (let index = 0; index < sorted.length; index += 1) {
    const metric = sorted[index] ?? 0
    if (!lessCountByMetric.has(metric)) lessCountByMetric.set(metric, index)
  }
  const denominator = Math.max(1, total - 1)

  return (node: GraphNode) => {
    const lessCount = lessCountByMetric.get(node.edgeCount) ?? 0
    const rank = lessCount / denominator
    return tier.min + span * rank
  }
}

type EmphasisLevel = 'selected' | 'neighbor' | 'hovered' | 'hoverNeighbor'

/**
 * Interaction emphasis is RELATIVE to the node's own base size:
 *   - `floor` keeps a selected/hovered node clearly visible even in a dense
 *     ~2 px sea (a hard minimum radius while focused);
 *   - `mult` makes an already-large sparse-graph hub visibly grow, which the
 *     old absolute `Math.max(size, 9)` did not — a 13 px hub stayed 13 px on
 *     selection, so nothing appeared to happen.
 */
const NODE_SIZE_EMPHASIS: Record<EmphasisLevel, { floor: number; mult: number }> = {
  selected: { floor: 9, mult: 1.7 },
  neighbor: { floor: 6.5, mult: 1.4 },
  hovered: { floor: 11, mult: 1.9 },
  hoverNeighbor: { floor: 7.5, mult: 1.45 },
}

export function emphasizeNodeSize(baseSize: number, level: EmphasisLevel): number {
  const { floor, mult } = NODE_SIZE_EMPHASIS[level]
  return Math.max(floor, baseSize * mult)
}
