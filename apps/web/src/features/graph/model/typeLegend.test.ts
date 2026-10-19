import { describe, expect, it } from 'vitest'
import type { GraphNode, GraphNodeType } from '@/shared/types'
import {
  EMPTY_LEGEND_FILTER,
  NO_SUBTYPE_KEY,
  buildTypeLegend,
  isNodeHiddenByLegend,
  isolateLegendSubType,
  isolateLegendType,
  legendSubtypes,
  subtypeFilterKey,
  toggleLegendSubType,
  toggleLegendType,
  type LegendFilter,
} from './typeLegend'

function node(id: string, type: GraphNodeType, subType?: string): GraphNode {
  return { id, label: id, type, properties: {}, edgeCount: 0, ...(subType ? { subType } : {}) }
}

const nodes = [
  node('alice', 'person', 'engineer'),
  node('bob', 'person', 'engineer'),
  node('carol', 'person', 'manager'),
  node('dave', 'person'),
  node('acme', 'organization'),
  node('doc', 'document'),
]
const legend = buildTypeLegend(nodes)

function visibleIds(filter: LegendFilter): string[] {
  return nodes.filter((item) => !isNodeHiddenByLegend(filter, item)).map((item) => item.id)
}

describe('legendSubtypes', () => {
  it('orders sub-types by frequency and appends the no-sub-type bucket', () => {
    expect(legendSubtypes(legend.get('person'))).toEqual([
      ['engineer', 2],
      ['manager', 1],
      [NO_SUBTYPE_KEY, 1],
    ])
  })

  it('has no rows for a type without explicit sub-types', () => {
    expect(legendSubtypes(legend.get('organization'))).toEqual([])
  })
})

describe('legend toggles', () => {
  it('hides a type together with all of its sub-types and brings it back', () => {
    const hidden = toggleLegendType(EMPTY_LEGEND_FILTER, 'person')
    expect(visibleIds(hidden)).toEqual(['acme', 'doc'])
    expect(visibleIds(toggleLegendType(hidden, 'person'))).toEqual(visibleIds(EMPTY_LEGEND_FILTER))
  })

  it('hides a single sub-type of a type', () => {
    const hidden = toggleLegendSubType(EMPTY_LEGEND_FILTER, subtypeFilterKey('person', 'engineer'))
    expect(visibleIds(hidden)).toEqual(['carol', 'dave', 'acme', 'doc'])
  })

  it('hides the no-sub-type bucket separately', () => {
    const hidden = toggleLegendSubType(EMPTY_LEGEND_FILTER, subtypeFilterKey('person'))
    expect(visibleIds(hidden)).toEqual(['alice', 'bob', 'carol', 'acme', 'doc'])
  })
})

describe('legend isolation', () => {
  it('shows only the chosen type with all of its sub-types', () => {
    const partlyHidden = toggleLegendSubType(
      EMPTY_LEGEND_FILTER,
      subtypeFilterKey('person', 'manager'),
    )
    const isolated = isolateLegendType(partlyHidden, legend, 'person')
    expect(visibleIds(isolated)).toEqual(['alice', 'bob', 'carol', 'dave'])
  })

  it('shows everything again when the isolated type is isolated once more', () => {
    const isolated = isolateLegendType(EMPTY_LEGEND_FILTER, legend, 'person')
    expect(isolateLegendType(isolated, legend, 'person')).toBe(EMPTY_LEGEND_FILTER)
  })

  it('re-isolates the whole type when only one of its sub-types was visible', () => {
    const subIsolated = isolateLegendSubType(
      EMPTY_LEGEND_FILTER,
      legend,
      'person',
      subtypeFilterKey('person', 'engineer'),
    )
    expect(visibleIds(isolateLegendType(subIsolated, legend, 'person'))).toEqual([
      'alice',
      'bob',
      'carol',
      'dave',
    ])
  })

  it('shows only the chosen sub-type', () => {
    const isolated = isolateLegendSubType(
      EMPTY_LEGEND_FILTER,
      legend,
      'person',
      subtypeFilterKey('person', 'engineer'),
    )
    expect(visibleIds(isolated)).toEqual(['alice', 'bob'])
  })

  it('shows everything again when the isolated sub-type is isolated once more', () => {
    const key = subtypeFilterKey('person', 'manager')
    const isolated = isolateLegendSubType(EMPTY_LEGEND_FILTER, legend, 'person', key)
    expect(visibleIds(isolated)).toEqual(['carol'])
    expect(isolateLegendSubType(isolated, legend, 'person', key)).toBe(EMPTY_LEGEND_FILTER)
  })

  it('moves isolation from one sub-type to its sibling', () => {
    const first = isolateLegendSubType(
      EMPTY_LEGEND_FILTER,
      legend,
      'person',
      subtypeFilterKey('person', 'engineer'),
    )
    const second = isolateLegendSubType(first, legend, 'person', subtypeFilterKey('person'))
    expect(visibleIds(second)).toEqual(['dave'])
  })
})
