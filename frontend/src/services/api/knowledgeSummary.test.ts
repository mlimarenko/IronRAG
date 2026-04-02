import { beforeEach, describe, expect, it, vi } from 'vitest'

vi.mock('src/stores/shell', () => ({
  useShellStore: vi.fn(),
}))

import { useShellStore } from 'src/stores/shell'

import {
  KNOWLEDGE_SUMMARY_UNAVAILABLE_WARNING,
  fetchDocumentsSurface,
  fetchLibraryKnowledgeSummary,
} from './documents'
import { fetchGraphSurfaceHeartbeat } from './graph'
import { ApiClientError, apiHttp } from './http'

const mockUseShellStore = vi.mocked(useShellStore)

function mockContextStore(libraryId = 'lib-1') {
  mockUseShellStore.mockReturnValue({
    context: {
      activeLibrary: {
        id: libraryId,
      },
    },
    activeLibrary: {
      id: libraryId,
    },
  } as ReturnType<typeof useShellStore>)
}

function mockDocumentDetail(libraryId = 'lib-1') {
  return {
    document: {
      id: 'doc-1',
      workspace_id: 'ws-1',
      library_id: libraryId,
      external_key: 'doc-1.md',
      document_state: 'active',
      created_at: '2026-04-02T12:00:00Z',
    },
    file_name: 'doc-1.md',
    head: null,
    active_revision: null,
    readiness: null,
    readiness_summary: null,
    web_page_provenance: null,
    prepared_revision: null,
    prepared_segment_count: null,
    technical_fact_count: null,
    pipeline: {
      latest_mutation: null,
      latest_job: null,
    },
  }
}

describe('knowledge summary contract', () => {
  beforeEach(() => {
    vi.restoreAllMocks()
    mockContextStore()
  })

  it('maps the canonical library summary response into readiness and graph coverage truth', async () => {
    vi.spyOn(apiHttp, 'get').mockImplementation((path) => {
      if (path === '/knowledge/libraries/lib-1/summary') {
        return Promise.resolve({
          data: {
            libraryId: 'lib-1',
            documentCountsByReadiness: {
              processing: 1,
              graph_sparse: 2,
              graph_ready: 3,
            },
            graphReadyDocumentCount: 3,
            graphSparseDocumentCount: 2,
            typedFactDocumentCount: 4,
            updatedAt: '2026-04-02T12:34:56Z',
            latestGeneration: {
              generationId: 'gen-1',
            },
          },
        })
      }
      throw new Error(`Unexpected request: ${String(path)}`)
    })

    const summary = await fetchLibraryKnowledgeSummary('lib-1')

    expect(summary).not.toBeNull()
    expect(summary?.readinessSummary.documentCountsByReadiness).toEqual({
      processing: 1,
      readable: 0,
      graphSparse: 2,
      graphReady: 3,
      failed: 0,
    })
    expect(summary?.graphCoverage).toMatchObject({
      graphReadyDocumentCount: 3,
      graphSparseDocumentCount: 2,
      typedFactDocumentCount: 4,
      lastGenerationId: 'gen-1',
      updatedAt: '2026-04-02T12:34:56Z',
    })
  })

  it('keeps the documents surface available when summary fetch fails', async () => {
    vi.spyOn(apiHttp, 'get').mockImplementation((path) => {
      if (path === '/content/documents') {
        return Promise.resolve({
          data: [mockDocumentDetail()],
        })
      }
      if (path === '/billing/library-document-costs') {
        return Promise.resolve({
          data: [],
        })
      }
      if (path === '/knowledge/libraries/lib-1/summary') {
        return Promise.reject(
          new ApiClientError('summary unavailable', 503, 'upstream_unavailable'),
        )
      }
      throw new Error(`Unexpected request: ${String(path)}`)
    })

    const surface = await fetchDocumentsSurface()

    expect(surface.rows).toHaveLength(1)
    expect(surface.graphStatus).toBe('partial')
    expect(surface.graphWarning).toBe(KNOWLEDGE_SUMMARY_UNAVAILABLE_WARNING)
  })

  it('preserves the graph heartbeat fallback when summary fetch fails', async () => {
    vi.spyOn(apiHttp, 'get').mockImplementation((path) => {
      if (path === '/knowledge/libraries/lib-1/summary') {
        return Promise.reject(
          new ApiClientError('summary unavailable', 503, 'upstream_unavailable'),
        )
      }
      throw new Error(`Unexpected request: ${String(path)}`)
    })

    const heartbeat = await fetchGraphSurfaceHeartbeat('lib-1', 5, 2, {
      graphStatus: 'ready',
      convergenceStatus: 'current',
      graphGeneration: 42,
      graphGenerationState: 'graph_ready',
      lastBuiltAt: '2026-04-02T12:34:56Z',
      readinessSummary: {
        libraryId: 'lib-1',
        documentCountsByReadiness: {
          processing: 0,
          readable: 0,
          graphSparse: 0,
          graphReady: 3,
          failed: 0,
        },
        updatedAt: '2026-04-02T12:34:56Z',
      },
      graphCoverage: {
        libraryId: 'lib-1',
        graphReadyDocumentCount: 3,
        graphSparseDocumentCount: 0,
        typedFactDocumentCount: 3,
        lastGenerationId: 'gen-1',
        updatedAt: '2026-04-02T12:34:56Z',
      },
      warning: null,
    })

    expect(heartbeat).toMatchObject({
      graphStatus: 'ready',
      convergenceStatus: 'current',
      graphGeneration: 42,
      graphGenerationState: 'graph_ready',
      warning: KNOWLEDGE_SUMMARY_UNAVAILABLE_WARNING,
    })
    expect(heartbeat.readinessSummary?.documentCountsByReadiness.graphReady).toBe(3)
    expect(heartbeat.graphCoverage?.graphReadyDocumentCount).toBe(3)
  })
})
