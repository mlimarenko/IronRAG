import { describe, expect, it } from 'vitest'

import { mapGraphTopology } from './graphAdapter'

describe('graph topology adapter', () => {
  it('prioritizes modern layouts for dense large graphs', () => {
    const entityCount = 420
    const entities = Array.from({ length: entityCount }, (_, index) => ({
      entityId: `entity-${index}`,
      canonicalLabel: `entity-${index}`,
      entityType: 'concept',
      supportCount: 1,
    }))
    const relations = Array.from({ length: entityCount * 5 }, (_, index) => ({
      relationId: `relation-${index}`,
      subjectEntityId: `entity-${index % entityCount}`,
      objectEntityId: `entity-${(index * 17 + 1) % entityCount}`,
      predicate: 'p',
      supportCount: 1,
    }))

    const topology = mapGraphTopology({
      entities,
      relations,
      documents: [],
      documentLinks: [],
    })

    expect(topology.meta.recommendedLayout).toBe('hubs')
    expect(topology.meta.recommendedLayout).not.toBe('bands')
  })
})
