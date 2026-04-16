import type { GraphNode } from '@/types';

export const NO_SUBTYPE_KEY = '__no_subtype__';

export type SubtypeBucket = {
  count: number;
  subs: Map<string, number>;
  noSubtypeCount: number;
};

export type TypeLegendMap = Map<string, SubtypeBucket>;

/**
 * Walk the node list once and bucket nodes by type + subType.
 * Called from `useMemo` on every `allNodes` change so the legend renders
 * from a stable, pre-aggregated view instead of re-walking the list on
 * every paint.
 */
export function buildTypeLegend(nodes: GraphNode[]): TypeLegendMap {
  const map: TypeLegendMap = new Map();
  for (const n of nodes) {
    let entry = map.get(n.type);
    if (!entry) {
      entry = { count: 0, subs: new Map(), noSubtypeCount: 0 };
      map.set(n.type, entry);
    }
    entry.count += 1;
    if (n.subType && n.subType.trim().length > 0) {
      entry.subs.set(n.subType, (entry.subs.get(n.subType) ?? 0) + 1);
    } else {
      entry.noSubtypeCount += 1;
    }
  }
  return map;
}

/**
 * Canonical key format used by the legend to track "hide this (type, subType)"
 * checkbox state. Nodes without an explicit sub-type share a single bucket.
 */
export function subtypeFilterKey(type: string, subType?: string | null): string {
  return `${type}:${subType && subType.trim().length > 0 ? subType : NO_SUBTYPE_KEY}`;
}
