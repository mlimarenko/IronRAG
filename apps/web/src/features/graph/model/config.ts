import type { GraphNode } from '@/shared/types';
import { compactText } from '@/shared/lib/compactText';

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
};

export const GRAPH_EDGE_COLORS = {
  dense: 'rgba(148, 163, 184, 0.38)',
  denseLight: 'rgba(15, 23, 42, 0.34)',
  regular: 'rgba(100, 116, 139, 0.72)',
  muted: 'rgba(100, 116, 139, 0.42)',
  highlight: 'rgba(245, 158, 11, 0.9)',
} as const;

/// Above this rendered-edge count the graph view draws a stride-sampled
/// subset of edges instead of every edge (rasterising every edge is the
/// large-graph render bottleneck). Shared between SigmaGraph (which applies
/// it) and GraphPage (which shows the edge-density toggle).
export const GRAPH_EDGE_RENDER_CAP = 70000;

/// Denser overview cap used when the user asks for more visual edge context.
/// This intentionally remains capped: the full topology stays available to
/// selection, neighbors, and inspectors, but the canvas never attempts to draw
/// hundreds of thousands of lines on every Firefox frame.
export const GRAPH_EDGE_DENSE_RENDER_CAP = 110000;

/// Above this raw edge count, changing the rendered edge density is itself a
/// visible Sigma repaint spike in Firefox. The full topology remains available
/// to adjacency, selection, and inspectors; the canvas keeps the safe overview
/// sample instead of exposing a janky toolbar action.
export const GRAPH_EDGE_DENSITY_TOGGLE_MAX_EDGES = 120000;

export const GRAPH_LAYOUT_OPTIONS = [
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
] as const;

export type GraphLayoutType = (typeof GRAPH_LAYOUT_OPTIONS)[number]['id'];

export const DEFAULT_GRAPH_LAYOUT: GraphLayoutType = 'hubs';

export function isGraphLayoutType(value: string | undefined | null): value is GraphLayoutType {
  return GRAPH_LAYOUT_OPTIONS.some((layout) => layout.id === value);
}

export function normalizeRecommendedGraphLayout(
  value: string | undefined | null,
): GraphLayoutType | null {
  if (!isGraphLayoutType(value)) return null;
  return value === 'bands' ? DEFAULT_GRAPH_LAYOUT : value;
}


function graphLabelBudget(nodeCount: number): number {
  if (nodeCount > 1200) return 6;
  if (nodeCount > 700) return 8;
  if (nodeCount > 350) return 10;
  if (nodeCount > 180) return 14;
  return 20;
}

function graphCanvasLabelLimit(nodeCount: number): number {
  if (nodeCount > 900) return 16;
  if (nodeCount > 450) return 18;
  return 22;
}

export function selectProminentGraphLabelIds(nodes: GraphNode[]): Set<string> {
  const ranked = [...nodes].sort((left, right) => {
    const edgeCountDelta = right.edgeCount - left.edgeCount;
    if (edgeCountDelta !== 0) return edgeCountDelta;

    const typeDelta = left.type.localeCompare(right.type);
    if (typeDelta !== 0) return typeDelta;

    return left.label.localeCompare(right.label);
  });

  return new Set(ranked.slice(0, graphLabelBudget(nodes.length)).map((node) => node.id));
}

export function buildGraphCanvasLabel(label: string, nodeCount: number): string {
  return compactText(label, graphCanvasLabelLimit(nodeCount)).text;
}

export function buildGraphFocusLabel(label: string): string {
  return compactText(label, 30).text;
}
