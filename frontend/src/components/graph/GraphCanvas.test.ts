import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

const graphCanvasPath = fileURLToPath(new URL('./GraphCanvas.vue', import.meta.url))

describe('GraphCanvas structure sync contract', () => {
  it('preserves live node coordinates during non-relayout sync and seeds only new nodes', () => {
    const source = readFileSync(graphCanvasPath, 'utf8')

    expect(source).toContain('function resolveSyncSeedPosition(')
    expect(source).toContain('applyTargetPositions: Boolean(options?.relayout)')
    expect(source).toContain(
      'x: options.applyTargetPositions ? targetAttributes.x : currentAttributes.x',
    )
    expect(source).toContain(
      'y: options.applyTargetPositions ? targetAttributes.y : currentAttributes.y',
    )
    expect(source).toContain('const seededPosition = options.applyTargetPositions')
    expect(source).toContain(': resolveSyncSeedPosition(nodeId, graph, targetGraph)')
  })
})
