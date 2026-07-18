export type LayoutNode = { id: string }

type ChunkedEdgeRefresher = (edgeIds: string[]) => void
type AnimationFrameRequest = (callback: FrameRequestCallback) => number
type AnimationFrameCancel = (handle: number) => void

type TooltipData = {
  nodeId: string
  label: string
  neighborLabels: string[]
  neighborCount: number
}

export function createFrameScheduler(
  requestFrame: AnimationFrameRequest,
  cancelFrame: AnimationFrameCancel,
): { schedule: (callback: FrameRequestCallback) => void; cancel: () => void } {
  let frame: number | null = null

  return {
    schedule(callback) {
      if (frame != null) return
      frame = requestFrame((timestamp) => {
        frame = null
        callback(timestamp)
      })
    },
    cancel() {
      if (frame == null) return
      cancelFrame(frame)
      frame = null
    },
  }
}

export function buildTooltipData({
  nodeId,
  neighbors,
  labelByNodeId,
  nodeLabel,
  maxNeighbors,
}: {
  nodeId: string
  neighbors: ReadonlySet<string> | undefined
  labelByNodeId: ReadonlyMap<string, string>
  nodeLabel: string
  maxNeighbors: number
}): TooltipData {
  const neighborLabels = Array.from(neighbors ?? [])
    .slice(0, maxNeighbors)
    .map((id) => labelByNodeId.get(id) ?? id)

  return {
    nodeId,
    label: nodeLabel,
    neighborLabels,
    neighborCount: neighbors?.size ?? 0,
  }
}

export async function updatePositionTextureFromLayoutChunked({
  positionTextureData,
  nodeIndexById,
  layoutNodes,
  positions,
  chunkSize,
  isCurrent,
  waitForFrame,
}: {
  positionTextureData: Float32Array
  nodeIndexById: ReadonlyMap<string, number>
  layoutNodes: readonly LayoutNode[]
  positions: ArrayLike<number>
  chunkSize: number
  isCurrent: () => boolean
  waitForFrame: () => Promise<void>
}): Promise<boolean> {
  for (let startIndex = 0; startIndex < layoutNodes.length; startIndex += chunkSize) {
    if (!isCurrent()) return false
    updatePositionTextureFromLayout(
      positionTextureData,
      nodeIndexById,
      layoutNodes,
      positions,
      startIndex,
      Math.min(layoutNodes.length, startIndex + chunkSize),
    )
    await waitForFrame()
  }

  return isCurrent()
}

export function createChunkedEdgeRestorer({
  edgeIds,
  chunkSize,
  isCurrent,
  refresh,
  restore,
  schedule,
}: {
  edgeIds: readonly string[]
  chunkSize: number
  isCurrent: () => boolean
  refresh: (edgeIds: string[], includeNode: boolean) => boolean
  restore: () => void
  schedule: AnimationFrameRequest
}): { start: () => void } {
  let offset = 0
  const refreshNextChunk: FrameRequestCallback = () => {
    if (!isCurrent()) {
      restore()
      return
    }
    const edgeChunk = edgeIds.slice(offset, offset + chunkSize)
    const includeNode = offset === 0
    offset += chunkSize
    if (!refresh(edgeChunk, includeNode)) {
      restore()
      return
    }
    if (offset < edgeIds.length) {
      schedule(refreshNextChunk)
      return
    }
    restore()
  }

  return { start: () => schedule(refreshNextChunk) }
}

export async function applyLayoutPositionsChunked({
  layoutNodes,
  positions,
  chunkSize,
  isCurrent,
  apply,
  refresh,
  waitForFrame,
}: {
  layoutNodes: readonly LayoutNode[]
  positions: ArrayLike<number>
  chunkSize: number
  isCurrent: () => boolean
  apply: (id: string, x: number | undefined, y: number | undefined) => void
  refresh: ChunkedEdgeRefresher
  waitForFrame: () => Promise<void>
}): Promise<boolean> {
  for (let offset = 0; offset < layoutNodes.length; offset += chunkSize) {
    if (!isCurrent()) return false
    const chunk = layoutNodes.slice(offset, offset + chunkSize)
    for (let index = 0; index < chunk.length; index += 1) {
      const node = chunk[index]
      if (!node) continue
      const sourceIndex = offset + index
      apply(node.id, positions[sourceIndex * 2], positions[sourceIndex * 2 + 1])
    }
    refresh(chunk.map((node) => node.id))
    await waitForFrame()
  }

  return isCurrent()
}

export async function refreshEdgeChunks({
  edgeIds,
  chunkSize,
  isCurrent,
  refresh,
  waitForFrame,
}: {
  edgeIds: readonly string[]
  chunkSize: number
  isCurrent: () => boolean
  refresh: ChunkedEdgeRefresher
  waitForFrame: () => Promise<void>
}): Promise<boolean> {
  for (let offset = 0; offset < edgeIds.length; offset += chunkSize) {
    if (!isCurrent()) return false
    refresh(edgeIds.slice(offset, offset + chunkSize))
    await waitForFrame()
  }
  return isCurrent()
}

export function createStaleGuard(isCurrent: () => boolean): () => boolean {
  return () => isCurrent()
}

export function updatePositionTextureFromLayout(
  positionTextureData: Float32Array,
  nodeIndexById: ReadonlyMap<string, number>,
  layoutNodes: readonly LayoutNode[],
  positions: ArrayLike<number>,
  startIndex = 0,
  endIndex = layoutNodes.length,
): void {
  for (let sourceIndex = startIndex; sourceIndex < endIndex; sourceIndex += 1) {
    const layoutNode = layoutNodes[sourceIndex]
    if (!layoutNode) continue
    const nodeIndex = nodeIndexById.get(layoutNode.id)
    if (nodeIndex == null) continue
    const offset = nodeIndex * 4
    positionTextureData[offset] = positions[sourceIndex * 2] ?? 0
    positionTextureData[offset + 1] = positions[sourceIndex * 2 + 1] ?? 0
  }
}
