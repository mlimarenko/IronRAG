import type { GraphStatus } from '@/shared/types'

export type StreamPhase = 'loading' | 'ready' | 'error'

/**
 * What the graph screen should claim while a stream is in flight. Two things
 * are being combined: whether the stream itself is still running or failed,
 * and what the server already knows about the stored graph. A stream that is
 * loading over a graph the server has already counted is a *re*build — saying
 * "building" there tells an operator their existing graph is gone.
 */
export function resolveGraphStatus(
  input: Readonly<{
    streamPhase: StreamPhase
    knownNodeCount: number
    serverStatus: GraphStatus | null
    loadedNodeCount: number
  }>,
): GraphStatus {
  if (input.streamPhase === 'loading') {
    return input.knownNodeCount > 0 ? 'rebuilding' : 'building'
  }
  if (input.streamPhase === 'error') return 'failed'
  if (input.serverStatus) return input.serverStatus
  return input.loadedNodeCount > 0 ? 'ready' : 'empty'
}
