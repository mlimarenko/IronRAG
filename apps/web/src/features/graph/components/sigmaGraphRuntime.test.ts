import { describe, expect, it, vi } from 'vitest'

import {
  applyLayoutPositionsChunked,
  buildTooltipData,
  createChunkedEdgeRestorer,
  createFrameScheduler,
  createStaleGuard,
  refreshEdgeChunks,
  updatePositionTextureFromLayout,
  updatePositionTextureFromLayoutChunked,
} from './sigmaGraphRuntime'

describe('SigmaGraph runtime helpers', () => {
  it('does not apply stale asynchronous layout work', () => {
    const isCurrent = vi.fn(() => false)

    expect(createStaleGuard(isCurrent)()).toBe(false)
    expect(isCurrent).toHaveBeenCalledOnce()
  })

  it('stops chunked refreshes when a layout switch becomes stale', async () => {
    const refresh = vi.fn()
    const waitForFrame = vi.fn(async () => undefined)
    const isCurrent = vi.fn(() => refresh.mock.calls.length === 0)

    await expect(
      refreshEdgeChunks({
        edgeIds: ['a', 'b', 'c'],
        chunkSize: 1,
        isCurrent,
        refresh,
        waitForFrame,
      }),
    ).resolves.toBe(false)

    expect(refresh).toHaveBeenCalledTimes(1)
    expect(refresh).toHaveBeenCalledWith(['a'])
  })

  it('writes only indexed nodes from a layout result', () => {
    const positionTextureData = new Float32Array(8)
    const nodeIndexById = new Map([
      ['first', 0],
      ['third', 1],
    ])

    updatePositionTextureFromLayout(
      positionTextureData,
      nodeIndexById,
      [{ id: 'first' }, { id: 'missing' }, { id: 'third' }],
      [1, 2, 3, 4, 5, 6],
    )

    expect(Array.from(positionTextureData)).toEqual([1, 2, 0, 0, 5, 6, 0, 0])
  })

  it('stops chunked texture updates before mutating stale work', async () => {
    const texture = new Float32Array(8)
    const isCurrent = vi.fn(() => false)

    await expect(
      updatePositionTextureFromLayoutChunked({
        positionTextureData: texture,
        nodeIndexById: new Map([['first', 0]]),
        layoutNodes: [{ id: 'first' }],
        positions: [3, 4],
        chunkSize: 1,
        isCurrent,
        waitForFrame: vi.fn(async () => undefined),
      }),
    ).resolves.toBe(false)

    expect(Array.from(texture)).toEqual([0, 0, 0, 0, 0, 0, 0, 0])
  })

  it('keeps only the latest scheduled frame and cancels it on cleanup', () => {
    const requestFrame = vi.fn(() => 42)
    const cancelFrame = vi.fn()
    const scheduler = createFrameScheduler(requestFrame, cancelFrame)
    const draw = vi.fn()

    scheduler.schedule(draw)
    scheduler.schedule(draw)
    scheduler.cancel()

    expect(requestFrame).toHaveBeenCalledOnce()
    expect(cancelFrame).toHaveBeenCalledWith(42)
    expect(draw).not.toHaveBeenCalled()
  })

  it('applies positions in chunks and stops when the runtime is stale', async () => {
    const applied: string[] = []
    const refresh = vi.fn()
    const isCurrent = vi.fn(() => applied.length < 2)

    await expect(
      applyLayoutPositionsChunked({
        layoutNodes: [{ id: 'first' }, { id: 'second' }, { id: 'third' }],
        positions: [1, 2, 3, 4, 5, 6],
        chunkSize: 1,
        isCurrent,
        apply: (id, x, y) => applied.push(`${id}:${x},${y}`),
        refresh,
        waitForFrame: vi.fn(async () => undefined),
      }),
    ).resolves.toBe(false)

    expect(applied).toEqual(['first:1,2', 'second:3,4'])
    expect(refresh).toHaveBeenCalledWith(['first'])
    expect(refresh).toHaveBeenCalledWith(['second'])
  })

  it('restores dense edge chunks and always restores base layers', () => {
    const scheduled: FrameRequestCallback[] = []
    const restore = vi.fn()
    const refresh = vi.fn(() => true)
    const schedule = vi.fn((callback: FrameRequestCallback) => {
      scheduled.push(callback)
      return scheduled.length
    })
    const restorer = createChunkedEdgeRestorer({
      edgeIds: ['one', 'two'],
      chunkSize: 1,
      isCurrent: () => true,
      refresh,
      restore,
      schedule,
    })

    restorer.start()
    scheduled.shift()?.(0)
    scheduled.shift()?.(0)

    expect(refresh).toHaveBeenNthCalledWith(1, ['one'], true)
    expect(refresh).toHaveBeenNthCalledWith(2, ['two'], false)
    expect(restore).toHaveBeenCalledOnce()
  })

  it('builds a bounded tooltip from available graph data', () => {
    const tooltip = buildTooltipData({
      nodeId: 'focus',
      neighbors: new Set(['first', 'second', 'third']),
      labelByNodeId: new Map([
        ['first', 'One'],
        ['second', 'Two'],
      ]),
      nodeLabel: 'Focus',
      maxNeighbors: 2,
    })

    expect(tooltip).toEqual({
      nodeId: 'focus',
      label: 'Focus',
      neighborLabels: ['One', 'Two'],
      neighborCount: 3,
    })
  })
})
