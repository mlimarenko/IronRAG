import { describe, expect, it } from 'vitest'

import { layoutCacheKey, loadCachedPositions, storeCachedPositions } from './positionCache'

describe('positionCache', () => {
  it('namespaces cache entries by topology signature and layout', () => {
    expect(layoutCacheKey('123:45:abc', 'force')).toBe('123:45:abc:force')
    expect(layoutCacheKey('123:45:abc', 'hubs')).not.toBe(layoutCacheKey('123:45:abc', 'force'))
  })

  it('degrades to a cache miss when IndexedDB is unavailable', async () => {
    // jsdom provides no usable IndexedDB: both operations must fail soft.
    await expect(loadCachedPositions('key', 'force')).resolves.toBeNull()
    await expect(
      storeCachedPositions('key', 'force', new Float32Array([1, 2])),
    ).resolves.toBeUndefined()
  })
})
