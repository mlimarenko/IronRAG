import { afterEach, describe, expect, it, vi } from 'vitest'

import { knowledgeApi } from './knowledge'

function ndjsonStream(chunks: string[]): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder()

  return new ReadableStream<Uint8Array>({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(encoder.encode(chunk))
      controller.close()
    },
  })
}

describe('knowledgeApi.getGraphTopology', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('reconstructs graph topology across chunk boundaries and reports progress', async () => {
    const progress = vi.fn()
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(
        ndjsonStream([
          '{"s":"meta","node_count":3,"edge_count":2}\n{"s":"id_map","m":{"document-1":1,"entity-1":2,"entity-2":3}}\n{"s":"docs","d":[{"i":1,"t":"Document"}]}\n{"s":"nodes","d":[{"i":2,"l":"One","s":2}',
          ',{"i":3,"l":"Two"}]}\n{"s":"edges","d":[[2,3,"relates_to",4]]}\n{"s":"doc_links","d":[[1,2,"mentions",5]]}',
        ]),
        { status: 200 },
      ),
    )

    await expect(
      knowledgeApi.getGraphTopology('library-1', { onProgress: progress }),
    ).resolves.toEqual({
      documents: [{ documentId: 'document-1', title: 'Document' }],
      entities: [
        {
          aliases: [],
          ['\u0063anonicalLabel']: 'One',
          entityId: 'entity-1',
          entityState: 'active',
          summary: null,
          supportCount: 2,
        },
        {
          aliases: [],
          ['\u0063anonicalLabel']: 'Two',
          entityId: 'entity-2',
          entityState: 'active',
          summary: null,
          supportCount: 1,
        },
      ],
      relations: [
        {
          objectEntityId: 'entity-2',
          predicate: 'relates_to',
          subjectEntityId: 'entity-1',
          supportCount: 4,
        },
      ],
      documentLinks: [{ documentId: 'document-1', targetNodeId: 'entity-1', supportCount: 5 }],
    })
    expect(globalThis.fetch).toHaveBeenCalledWith('/v1/knowledge/libraries/library-1/graph', {
      credentials: 'include',
      headers: { Accept: 'application/x-ndjson' },
    })
    expect(progress).toHaveBeenLastCalledWith({
      nodesLoaded: 3,
      edgesLoaded: 2,
      expectedNodes: 3,
      expectedEdges: 2,
    })
  })

  it('reconstructs the expected unpublished empty topology', async () => {
    const progress = vi.fn()
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(
        ndjsonStream([
          '{"s":"meta","v":1,"library_id":"library-1","projection_version":1,"topology_generation":0,"node_count":0,"edge_count":0,"document_count":0}\n',
          '{"s":"id_map","m":{}}\n{"s":"end"}\n',
        ]),
        { status: 200 },
      ),
    )

    await expect(
      knowledgeApi.getGraphTopology('library-1', { onProgress: progress }),
    ).resolves.toEqual({ documents: [], entities: [], relations: [], documentLinks: [] })
    expect(progress).toHaveBeenLastCalledWith({
      nodesLoaded: 0,
      edgesLoaded: 0,
      expectedNodes: 0,
      expectedEdges: 0,
    })
  })

  it('uses structured response details in graph endpoint errors', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(JSON.stringify({ code: 'graph_unavailable', detail: 'Graph unavailable' }), {
        status: 503,
      }),
    )

    await expect(knowledgeApi.getGraphTopology('library-1')).rejects.toMatchObject({
      status: 503,
      body: { code: 'graph_unavailable', detail: 'Graph unavailable', status: 503 },
      message: 'Graph unavailable',
    })
  })
})
