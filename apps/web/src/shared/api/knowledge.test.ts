import { afterEach, describe, expect, it, vi } from 'vitest'
import { knowledgeApi } from './knowledge'

const receipt = {
  id: 'prep',
  libraryId: 'library-1',
  formatVersion: 1,
  projectionVersion: 2,
  topologyGeneration: 3,
  sourceTruthVersion: 4,
  status: 'ready',
  attempts: 1,
  retryAfterMs: 100,
  expiresAt: null,
  errorCode: null,
}
const meta = {
  s: 'meta',
  v: 1,
  library_id: 'library-1',
  preparation_id: 'prep',
  projection_version: 2,
  topology_generation: 3,
  source_truth_version: 4,
  generated_at: '2026-01-01',
  node_count: 2,
  edge_count: 1,
  document_count: 1,
}
const end = { s: 'end', preparation_id: 'prep', node_count: 2, edge_count: 1, document_count: 1 }
const frames = [
  meta,
  { s: 'id_map', m: { doc: 1 } },
  { s: 'id_map', m: { entity: 2 } },
  { s: 'docs', d: [{ i: 1 }] },
  { s: 'nodes', d: [{ i: 2, l: 'Entity' }] },
  { s: 'doc_links', d: [[1, 2, 'mentions', 1]] },
  end,
]
const stream = (items: unknown[]) =>
  new Response(items.map((x) => JSON.stringify(x)).join('\n') + '\n')

describe('graph preparation and stream', () => {
  afterEach(() => {
    vi.restoreAllMocks()
    vi.useRealTimers()
  })
  it('posts, polls and installs a complete batched stream', async () => {
    const fetch = vi
      .spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(Response.json({ ...receipt, status: 'queued' }))
      .mockResolvedValueOnce(Response.json(receipt))
      .mockResolvedValueOnce(stream(frames))
    const graph = await knowledgeApi.getGraphTopology('library-1')
    expect(graph.documents).toHaveLength(1)
    expect(graph.entities).toHaveLength(1)
    expect(graph.documentLinks).toHaveLength(1)
    expect(fetch.mock.calls.map(([url]) => url)).toEqual([
      '/v1/knowledge/libraries/library-1/graph/preparations',
      '/v1/knowledge/libraries/library-1/graph/preparations/prep',
      '/v1/knowledge/libraries/library-1/graph/preparations/prep/result',
    ])
    expect(fetch.mock.calls[0]?.[1]?.method).toBe('POST')
  })
  it('bounds zero-interval polls and proceeds through active phases', async () => {
    vi.useFakeTimers()
    const fetch = vi
      .spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(Response.json({ ...receipt, status: 'queued', retryAfterMs: 0 }))
      .mockResolvedValueOnce(Response.json({ ...receipt, status: 'building', retryAfterMs: 0 }))
      .mockResolvedValueOnce(Response.json(receipt))
      .mockResolvedValueOnce(stream(frames))
    const result = knowledgeApi.getGraphTopology('library-1')
    await vi.advanceTimersByTimeAsync(999)
    expect(fetch).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(1000)
    expect(fetch).toHaveBeenCalledTimes(2)
    await vi.advanceTimersByTimeAsync(1)
    expect((await result).entities).toHaveLength(1)
    expect(fetch).toHaveBeenCalledTimes(4)
  })
  it('cancels a pending zero-interval wait on abort', async () => {
    vi.useFakeTimers()
    const fetch = vi
      .spyOn(globalThis, 'fetch')
      .mockResolvedValue(Response.json({ ...receipt, status: 'building', retryAfterMs: 0 }))
    const controller = new AbortController()
    const result = knowledgeApi.getGraphTopology('library-1', { signal: controller.signal })
    const rejected = expect(result).rejects.toMatchObject({ name: 'AbortError' })
    await vi.advanceTimersByTimeAsync(0)
    expect(vi.getTimerCount()).toBe(1)
    controller.abort()
    await rejected
    expect(vi.getTimerCount()).toBe(0)
    await vi.advanceTimersByTimeAsync(2000)
    expect(fetch).toHaveBeenCalledOnce()
  })
  it('downloads an immediately ready result without polling or waiting', async () => {
    vi.useFakeTimers()
    const fetch = vi
      .spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(Response.json(receipt))
      .mockResolvedValueOnce(stream(frames))
    expect((await knowledgeApi.getGraphTopology('library-1')).entities).toHaveLength(1)
    expect(fetch).toHaveBeenCalledTimes(2)
    expect(fetch.mock.calls[1]?.[0]).toBe(
      '/v1/knowledge/libraries/library-1/graph/preparations/prep/result',
    )
    expect(vi.getTimerCount()).toBe(0)
  })
  it.each(['failed', 'expired', 'superseded'])(
    'stops on %s without re-admission',
    async (status) => {
      const fetch = vi
        .spyOn(globalThis, 'fetch')
        .mockResolvedValue(Response.json({ ...receipt, status }))
      await expect(knowledgeApi.getGraphTopology('library-1')).rejects.toThrow(status)
      expect(fetch).toHaveBeenCalledOnce()
    },
  )
  it.each([
    { ...receipt, expiresAt: undefined },
    { ...receipt, errorCode: undefined },
    { ...receipt, expiresAt: 1 },
    { ...receipt, errorCode: 1 },
  ])('rejects invalid nullable preparation fields', async (invalid) => {
    const fetch = vi.spyOn(globalThis, 'fetch').mockResolvedValue(Response.json(invalid))
    await expect(knowledgeApi.getGraphTopology('library-1')).rejects.toThrow(
      'Invalid graph preparation receipt',
    )
    expect(fetch).toHaveBeenCalledOnce()
  })
  it.each([
    ['wrong identity', [{ ...meta, source_truth_version: 9 }, ...frames.slice(1)]],
    ['missing end', frames.slice(0, -1)],
    ['duplicate end', [...frames, end]],
    ['wrong count', [...frames.slice(0, -1), { ...end, node_count: 3 }]],
    ['unknown reference', [...frames.slice(0, 5), { s: 'doc_links', d: [[1, 9, 'x', 1]] }, end]],
    ['out of order', [meta, frames[3], frames[1], end]],
  ])('rejects %s', async (_name, invalid) => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(Response.json(receipt))
      .mockResolvedValueOnce(stream(invalid))
    await expect(knowledgeApi.getGraphTopology('library-1')).rejects.toThrow()
  })
  it.each(['', '  \t'])('rejects a blank or whitespace-only frame %j', async (blank) => {
    const data = frames.map((frame) => JSON.stringify(frame)).join('\n') + `\n${blank}\n`
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(Response.json(receipt))
      .mockResolvedValueOnce(new Response(data))
    await expect(knowledgeApi.getGraphTopology('library-1')).rejects.toThrow('Empty graph frame')
  })
  it('rejects malformed UTF-8 and truncated terminal frames', async () => {
    const fetch = vi.spyOn(globalThis, 'fetch')
    fetch
      .mockResolvedValueOnce(Response.json(receipt))
      .mockResolvedValueOnce(new Response(new Uint8Array([0xff, 10])))
    await expect(knowledgeApi.getGraphTopology('library-1')).rejects.toThrow()
    fetch
      .mockResolvedValueOnce(Response.json(receipt))
      .mockResolvedValueOnce(new Response(frames.map((x) => JSON.stringify(x)).join('\n')))
    await expect(knowledgeApi.getGraphTopology('library-1')).rejects.toThrow('Truncated')
  })
  it('cancels an open result stream on navigation', async () => {
    const cancel = vi.fn()
    const body = new ReadableStream<Uint8Array>({ cancel })
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(Response.json(receipt))
      .mockResolvedValueOnce(new Response(body))
    const controller = new AbortController()
    const result = knowledgeApi.getGraphTopology('library-1', { signal: controller.signal })
    const rejected = expect(result).rejects.toMatchObject({ name: 'AbortError' })
    await vi.waitFor(() => expect(body.locked).toBe(true))
    controller.abort()
    await rejected
    expect(cancel).toHaveBeenCalledOnce()
    expect(body.locked).toBe(false)
  })
  it('aborts polling locally without posting again', async () => {
    const fetch = vi
      .spyOn(globalThis, 'fetch')
      .mockResolvedValue(Response.json({ ...receipt, status: 'building', retryAfterMs: 30000 }))
    const controller = new AbortController()
    const result = knowledgeApi.getGraphTopology('library-1', { signal: controller.signal })
    await vi.waitFor(() => expect(fetch).toHaveBeenCalledOnce())
    controller.abort()
    await expect(result).rejects.toMatchObject({ name: 'AbortError' })
    expect(fetch).toHaveBeenCalledOnce()
  })
  it('rejects an already aborted request without admission', async () => {
    const fetch = vi.spyOn(globalThis, 'fetch')
    const controller = new AbortController()
    controller.abort()
    await expect(
      knowledgeApi.getGraphTopology('library-1', { signal: controller.signal }),
    ).rejects.toMatchObject({ name: 'AbortError' })
    expect(fetch).not.toHaveBeenCalled()
  })
  it('retains structured HTTP errors', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(JSON.stringify({ code: 'graph_unavailable', detail: 'Graph unavailable' }), {
        status: 503,
      }),
    )
    await expect(knowledgeApi.getGraphTopology('library-1')).rejects.toMatchObject({
      status: 503,
      body: { code: 'graph_unavailable', detail: 'Graph unavailable', status: 503 },
    })
  })
})
