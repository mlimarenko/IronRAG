import { compactText } from '@/shared/lib/compactText'

export const GRAPH_NODE_COLORS: Record<string, string> = {
  document: '#3b82f6',
  person: '#ec4899',
  organization: '#64748b',
  location: '#84cc16',
  event: '#f43f5e',
  artifact: '#06b6d4',
  natural: '#22c55e',
  process: '#a855f7',
  concept: '#f59e0b',
  attribute: '#0ea5e9',
  entity: '#78716c',
}

export const GRAPH_EDGE_COLORS = {
  dense: 'rgba(148, 163, 184, 0.38)',
  denseLight: 'rgba(15, 23, 42, 0.34)',
  regular: 'rgba(100, 116, 139, 0.72)',
  muted: 'rgba(100, 116, 139, 0.42)',
  highlight: 'rgba(245, 158, 11, 0.9)',
  hover: 'rgba(56, 189, 248, 0.95)',
} as const

/// Above this edge count the canvas draws only the strongest edges by
/// default (the scene edge buffer is sorted by weight descending, so the cap
/// is a "most important K", not a sample). The edge-density toggle lifts the
/// cap to the full edge set. Selection, neighbor highlighting, and the
/// inspector always see the complete topology regardless of the cap.
export const GRAPH_EDGE_RENDER_CAP = 200_000

export const GRAPH_LAYOUT_OPTIONS = [
  {
    id: 'force',
    iconKey: 'force',
    labelKey: 'graph.layouts.force',
    descriptionKey: 'graph.layoutDescriptions.force',
  },
  {
    id: 'hubs',
    iconKey: 'hubs',
    labelKey: 'graph.layouts.hubs',
    descriptionKey: 'graph.layoutDescriptions.hubs',
  },
  {
    id: 'sources',
    iconKey: 'sources',
    labelKey: 'graph.layouts.sources',
    descriptionKey: 'graph.layoutDescriptions.sources',
  },
  {
    id: 'flow',
    iconKey: 'flow',
    labelKey: 'graph.layouts.flow',
    descriptionKey: 'graph.layoutDescriptions.flow',
  },
  {
    id: 'radial',
    iconKey: 'radial',
    labelKey: 'graph.layouts.radial',
    descriptionKey: 'graph.layoutDescriptions.radial',
  },
  {
    id: 'circlepack',
    iconKey: 'circlepack',
    labelKey: 'graph.layouts.circlepack',
    descriptionKey: 'graph.layoutDescriptions.circlepack',
  },
  {
    id: 'sectors',
    iconKey: 'sectors',
    labelKey: 'graph.layouts.sectors',
    descriptionKey: 'graph.layoutDescriptions.sectors',
  },
  {
    id: 'bands',
    iconKey: 'bands',
    labelKey: 'graph.layouts.bands',
    descriptionKey: 'graph.layoutDescriptions.bands',
  },
  {
    id: 'components',
    iconKey: 'components',
    labelKey: 'graph.layouts.components',
    descriptionKey: 'graph.layoutDescriptions.components',
  },
  {
    id: 'rings',
    iconKey: 'rings',
    labelKey: 'graph.layouts.rings',
    descriptionKey: 'graph.layoutDescriptions.rings',
  },
  {
    id: 'clusters',
    iconKey: 'clusters',
    labelKey: 'graph.layouts.clusters',
    descriptionKey: 'graph.layoutDescriptions.clusters',
  },
] as const

export type GraphLayoutType = (typeof GRAPH_LAYOUT_OPTIONS)[number]['id']

export const DEFAULT_GRAPH_LAYOUT: GraphLayoutType = 'force'

/**
 * Iterative (force-directed) layouts must ALWAYS be computed off the main
 * thread: FA2 runs hundreds of passes, so a synchronous compute would stall
 * the UI. GraphCanvas routes these through the layout worker at every graph
 * size, not only past the node-count worker threshold.
 */
export function isIterativeLayout(layout: GraphLayoutType): boolean {
  return layout === 'force'
}

function graphCanvasLabelLimit(nodeCount: number): number {
  if (nodeCount > 900) return 16
  if (nodeCount > 450) return 18
  return 22
}

export function buildGraphCanvasLabel(label: string, nodeCount: number): string {
  return compactText(label, graphCanvasLabelLimit(nodeCount)).text
}

export function buildGraphFocusLabel(label: string): string {
  return compactText(label, 30).text
}
