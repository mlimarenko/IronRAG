/**
 * Persistent layout-position cache (IndexedDB). Keyed by the scene model's
 * content-derived topology signature plus the layout id, so reopening the
 * same library skips layout computation entirely — the settled positions
 * tween in straight from disk — while any topology change (new documents,
 * regenerated graph) naturally misses and recomputes.
 *
 * Everything here is fail-soft: environments without IndexedDB (tests,
 * private-mode browsers) and quota/IO errors degrade to "no cache".
 */

const DATABASE_NAME = 'ironrag-graph-layout'
const STORE_NAME = 'positions'
const DATABASE_VERSION = 1
/** Entries beyond this count are evicted oldest-first on write. */
const MAX_ENTRIES = 12

export function layoutCacheKey(topologyKey: string, layout: string): string {
  return `${topologyKey}:${layout}`
}

interface CacheRow {
  key: string
  savedAt: number
  positions: ArrayBuffer
}

function openDatabase(): Promise<IDBDatabase | null> {
  return new Promise((resolve) => {
    if (typeof indexedDB === 'undefined') {
      resolve(null)
      return
    }
    try {
      const request = indexedDB.open(DATABASE_NAME, DATABASE_VERSION)
      request.onupgradeneeded = () => {
        const database = request.result
        if (!database.objectStoreNames.contains(STORE_NAME)) {
          const store = database.createObjectStore(STORE_NAME, { keyPath: 'key' })
          store.createIndex('savedAt', 'savedAt')
        }
      }
      request.onsuccess = () => resolve(request.result)
      request.onerror = () => resolve(null)
      request.onblocked = () => resolve(null)
    } catch {
      resolve(null)
    }
  })
}

export async function loadCachedPositions(
  topologyKey: string,
  layout: string,
): Promise<Float32Array | null> {
  const database = await openDatabase()
  if (!database) return null
  return new Promise((resolve) => {
    try {
      const transaction = database.transaction(STORE_NAME, 'readonly')
      const request = transaction.objectStore(STORE_NAME).get(layoutCacheKey(topologyKey, layout))
      request.onsuccess = () => {
        const row = request.result as CacheRow | undefined
        resolve(row ? new Float32Array(row.positions) : null)
        database.close()
      }
      request.onerror = () => {
        resolve(null)
        database.close()
      }
    } catch {
      resolve(null)
      database.close()
    }
  })
}

export async function storeCachedPositions(
  topologyKey: string,
  layout: string,
  positions: Float32Array,
): Promise<void> {
  const database = await openDatabase()
  if (!database) return
  await new Promise<void>((resolve) => {
    try {
      const transaction = database.transaction(STORE_NAME, 'readwrite')
      const store = transaction.objectStore(STORE_NAME)
      const row: CacheRow = {
        key: layoutCacheKey(topologyKey, layout),
        savedAt: Date.now(),
        positions: positions.slice().buffer,
      }
      store.put(row)
      // Evict oldest entries beyond the cap so the cache cannot grow without
      // bound (a 230k-node layout is ~1.8 MB per entry).
      const countRequest = store.count()
      countRequest.onsuccess = () => {
        let excess = countRequest.result - MAX_ENTRIES
        if (excess <= 0) return
        const cursorRequest = store.index('savedAt').openCursor()
        cursorRequest.onsuccess = () => {
          const cursor = cursorRequest.result
          if (!cursor || excess <= 0) return
          cursor.delete()
          excess -= 1
          cursor.continue()
        }
      }
      transaction.oncomplete = () => {
        resolve()
        database.close()
      }
      transaction.onerror = () => {
        resolve()
        database.close()
      }
    } catch {
      resolve()
      database.close()
    }
  })
}
