import { describe, expect, it } from 'vitest'

import { resolveGraphStatus } from './graphStatus'

describe('resolveGraphStatus', () => {
  it('calls a load over a graph the server already counted a rebuild', () => {
    // "building" over an existing graph reads as "your graph is gone".
    expect(
      resolveGraphStatus({
        streamPhase: 'loading',
        knownNodeCount: 4200,
        serverStatus: 'ready',
        loadedNodeCount: 0,
      }),
    ).toBe('rebuilding')
  })

  it('calls a load with nothing stored a first build', () => {
    expect(
      resolveGraphStatus({
        streamPhase: 'loading',
        knownNodeCount: 0,
        serverStatus: null,
        loadedNodeCount: 0,
      }),
    ).toBe('building')
  })

  it('reports a failed stream regardless of what the server last knew', () => {
    expect(
      resolveGraphStatus({
        streamPhase: 'error',
        knownNodeCount: 10,
        serverStatus: 'ready',
        loadedNodeCount: 10,
      }),
    ).toBe('failed')
  })

  it('prefers the server status once the stream has settled', () => {
    expect(
      resolveGraphStatus({
        streamPhase: 'ready',
        knownNodeCount: 0,
        serverStatus: 'partial',
        loadedNodeCount: 3,
      }),
    ).toBe('partial')
  })

  it('falls back to what actually arrived when the server said nothing', () => {
    expect(
      resolveGraphStatus({
        streamPhase: 'ready',
        knownNodeCount: 0,
        serverStatus: null,
        loadedNodeCount: 3,
      }),
    ).toBe('ready')
    expect(
      resolveGraphStatus({
        streamPhase: 'ready',
        knownNodeCount: 0,
        serverStatus: null,
        loadedNodeCount: 0,
      }),
    ).toBe('empty')
  })
})
