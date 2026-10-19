import type { GraphNode } from '@/shared/types'

export const NO_SUBTYPE_KEY = '__no_subtype__'

type SubtypeBucket = {
  count: number
  subs: Map<string, number>
  noSubtypeCount: number
}

export type TypeLegendMap = Map<string, SubtypeBucket>

/**
 * Which legend entries the user switched off. A hidden type hides all of its
 * nodes regardless of `hiddenSubTypes`; a hidden sub-type key (see
 * `subtypeFilterKey`) hides only nodes of that (type, subType) pair.
 */
export type LegendFilter = Readonly<{
  hiddenTypes: ReadonlySet<string>
  hiddenSubTypes: ReadonlySet<string>
}>

export const EMPTY_LEGEND_FILTER: LegendFilter = {
  hiddenTypes: new Set(),
  hiddenSubTypes: new Set(),
}

/**
 * Walk the node list once and bucket nodes by type + subType.
 * Called from `useMemo` on every `allNodes` change so the legend renders
 * from a stable, pre-aggregated view instead of re-walking the list on
 * every paint.
 */
export function buildTypeLegend(nodes: GraphNode[]): TypeLegendMap {
  const map: TypeLegendMap = new Map()
  for (const n of nodes) {
    let entry = map.get(n.type)
    if (!entry) {
      entry = { count: 0, subs: new Map(), noSubtypeCount: 0 }
      map.set(n.type, entry)
    }
    entry.count += 1
    if (n.subType && n.subType.trim().length > 0) {
      entry.subs.set(n.subType, (entry.subs.get(n.subType) ?? 0) + 1)
    } else {
      entry.noSubtypeCount += 1
    }
  }
  return map
}

/**
 * Identity key format used by the legend to track "hide this (type, subType)"
 * checkbox state. Nodes without an explicit sub-type share a single bucket.
 */
export function subtypeFilterKey(type: string, subType?: string | null): string {
  return `${type}:${subType && subType.trim().length > 0 ? subType : NO_SUBTYPE_KEY}`
}

/**
 * Sub-type rows of one legend type, most frequent first. The "no sub-type"
 * bucket is a row only when the type has explicit sub-types beside it.
 */
export function legendSubtypes(bucket: SubtypeBucket | undefined): Array<[string, number]> {
  if (!bucket || bucket.subs.size === 0) return []
  const rows = Array.from(bucket.subs.entries()).sort((left, right) => right[1] - left[1])
  if (bucket.noSubtypeCount > 0) rows.push([NO_SUBTYPE_KEY, bucket.noSubtypeCount])
  return rows
}

function legendSubtypeKeys(legend: TypeLegendMap, type: string): string[] {
  return legendSubtypes(legend.get(type)).map(([subType]) => subtypeFilterKey(type, subType))
}

export function isNodeHiddenByLegend(filter: LegendFilter, node: GraphNode): boolean {
  return (
    filter.hiddenTypes.has(node.type) ||
    filter.hiddenSubTypes.has(subtypeFilterKey(node.type, node.subType))
  )
}

function toggled(current: ReadonlySet<string>, value: string): Set<string> {
  const next = new Set(current)
  if (next.has(value)) next.delete(value)
  else next.add(value)
  return next
}

/** Switch a type, together with all of its sub-types, off or back on. */
export function toggleLegendType(filter: LegendFilter, type: string): LegendFilter {
  return { ...filter, hiddenTypes: toggled(filter.hiddenTypes, type) }
}

/** Switch one sub-type of a type off or back on. */
export function toggleLegendSubType(filter: LegendFilter, key: string): LegendFilter {
  return { ...filter, hiddenSubTypes: toggled(filter.hiddenSubTypes, key) }
}

function otherTypes(legend: TypeLegendMap, type: string): string[] {
  return Array.from(legend.keys()).filter((candidate) => candidate !== type)
}

function isTypeIsolated(filter: LegendFilter, legend: TypeLegendMap, type: string): boolean {
  return (
    !filter.hiddenTypes.has(type) &&
    otherTypes(legend, type).every((other) => filter.hiddenTypes.has(other))
  )
}

/**
 * Show only `type` with all of its sub-types. Isolating the type that is
 * already the only visible entry brings everything back, as in Grafana.
 */
export function isolateLegendType(
  filter: LegendFilter,
  legend: TypeLegendMap,
  type: string,
): LegendFilter {
  const alreadyIsolated =
    isTypeIsolated(filter, legend, type) &&
    legendSubtypeKeys(legend, type).every((key) => !filter.hiddenSubTypes.has(key))
  if (alreadyIsolated) return EMPTY_LEGEND_FILTER
  return { hiddenTypes: new Set(otherTypes(legend, type)), hiddenSubTypes: new Set() }
}

/**
 * Show only the sub-type `key` of `type`. Isolating the sub-type that is
 * already the only visible entry brings everything back.
 */
export function isolateLegendSubType(
  filter: LegendFilter,
  legend: TypeLegendMap,
  type: string,
  key: string,
): LegendFilter {
  const siblings = legendSubtypeKeys(legend, type).filter((candidate) => candidate !== key)
  const alreadyIsolated =
    isTypeIsolated(filter, legend, type) &&
    !filter.hiddenSubTypes.has(key) &&
    siblings.every((sibling) => filter.hiddenSubTypes.has(sibling))
  if (alreadyIsolated) return EMPTY_LEGEND_FILTER
  return { hiddenTypes: new Set(otherTypes(legend, type)), hiddenSubTypes: new Set(siblings) }
}
