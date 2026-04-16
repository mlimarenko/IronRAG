import type { GraphNode } from '@/types';
import { compactText } from '@/lib/compactText';

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
  dense: 'rgba(71, 85, 105, 0.48)',
  regular: 'rgba(71, 85, 105, 0.64)',
  muted: 'rgba(71, 85, 105, 0.34)',
  highlight: 'rgba(51, 65, 85, 0.82)',
} as const;

export const GRAPH_LAYOUT_OPTIONS = [
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

export function isGraphLayoutType(value: string | undefined | null): value is GraphLayoutType {
  return GRAPH_LAYOUT_OPTIONS.some((layout) => layout.id === value);
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
