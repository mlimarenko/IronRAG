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
