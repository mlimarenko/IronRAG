import { afterEach, describe, expect, it, vi } from 'vitest'

import { KNOWLEDGE_SUMMARY_UNAVAILABLE_WARNING } from './documents'
import { fetchGraphSurface, fetchGraphSurfaceHeartbeat } from './graph'
import { ApiClientError, apiHttp } from './http'

describe('graph summary degradation contract', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('keeps graph topology available when only /summary fails', async () => {
    vi.spyOn(apiHttp, 'get').mockImplementation(async (path: string) => {
      if (path === '/knowledge/libraries/library-1/graph-topology') {
        return {
          data: {
            documents: [
              {
                key: 'doc-row',
                documentId: 'document-1',
                workspaceId: 'workspace-1',
                libraryId: 'library-1',
                externalKey: 'document-1',
                title: 'Document One',
                documentState: 'active',
                activeRevisionId: null,
                readableRevisionId: null,
                latestRevisionNo: null,
                createdAt: '2026-04-02T12:00:00Z',
                updatedAt: '2026-04-02T12:00:00Z',
                deletedAt: null,
              },
            ],
            entities: [],
            relations: [],
            documentLinks: [],
          },
        }
      }

      if (path === '/knowledge/libraries/library-1/summary') {
        throw new ApiClientError('Failed to load library knowledge summary', 503)
      }

      throw new Error(`Unexpected path: ${path}`)
    })

    const surface = await fetchGraphSurface('library-1')

    expect(surface.nodes).toHaveLength(1)
    expect(surface.graphStatus).toBe('partial')
    expect(surface.canvasMode).toBe('sparse')
    expect(surface.warning).toBe(KNOWLEDGE_SUMMARY_UNAVAILABLE_WARNING)
  })

  it('keeps the previous heartbeat when /summary fails during polling', async () => {
    vi.spyOn(apiHttp, 'get').mockRejectedValue(
      new ApiClientError('Failed to load library knowledge summary', 503),
    )

    const heartbeat = await fetchGraphSurfaceHeartbeat('library-1', 3, 2, {
      graphStatus: 'ready',
      convergenceStatus: 'current',
      graphGeneration: 1700000000000,
      graphGenerationState: 'graph_ready',
      lastBuiltAt: '2026-04-02T12:00:00Z',
      readinessSummary: {
        libraryId: 'library-1',
        documentCountsByReadiness: {
          processing: 0,
          readable: 0,
          graphSparse: 0,
          graphReady: 3,
          failed: 0,
        },
        updatedAt: '2026-04-02T12:00:00Z',
      },
      graphCoverage: {
        libraryId: 'library-1',
        graphReadyDocumentCount: 3,
        graphSparseDocumentCount: 0,
        typedFactDocumentCount: 3,
        lastGenerationId: 'generation-1',
        updatedAt: '2026-04-02T12:00:00Z',
      },
      warning: null,
    })

    expect(heartbeat.graphStatus).toBe('ready')
    expect(heartbeat.graphGenerationState).toBe('graph_ready')
    expect(heartbeat.warning).toBe(KNOWLEDGE_SUMMARY_UNAVAILABLE_WARNING)
  })
})
