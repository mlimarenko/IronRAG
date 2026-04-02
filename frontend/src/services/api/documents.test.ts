import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  buildEmptyLibraryKnowledgeSummary,
  fetchLibraryKnowledgeSummary,
  KNOWLEDGE_SUMMARY_UNAVAILABLE_WARNING,
  resolveLibraryKnowledgeSummaryProjection,
} from './documents'
import { ApiClientError, apiHttp } from './http'

describe('library knowledge summary contract', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('maps the canonical /summary payload into readiness and graph coverage truth', async () => {
    const getSpy = vi.spyOn(apiHttp, 'get').mockResolvedValue({
      data: {
        libraryId: 'library-1',
        documentCountsByReadiness: {
          processing: 1,
          graph_ready: 2,
        },
        graphReadyDocumentCount: 2,
        graphSparseDocumentCount: 1,
        typedFactDocumentCount: 3,
        updatedAt: '2026-04-02T12:00:00Z',
        latestGeneration: {
          generation_id: 'generation-1',
        },
      },
    })

    const summary = await fetchLibraryKnowledgeSummary('library-1')

    expect(getSpy).toHaveBeenCalledWith('/knowledge/libraries/library-1/summary')
    expect(summary).toEqual({
      libraryId: 'library-1',
      readinessSummary: {
        libraryId: 'library-1',
        documentCountsByReadiness: {
          processing: 1,
          readable: 0,
          graphSparse: 0,
          graphReady: 2,
          failed: 0,
        },
        updatedAt: '2026-04-02T12:00:00Z',
      },
      graphCoverage: {
        libraryId: 'library-1',
        graphReadyDocumentCount: 2,
        graphSparseDocumentCount: 1,
        typedFactDocumentCount: 3,
        lastGenerationId: 'generation-1',
        updatedAt: '2026-04-02T12:00:00Z',
      },
      latestGeneration: {
        generation_id: 'generation-1',
      },
    })
  })

  it('keeps the fallback snapshot when /summary is temporarily unavailable', async () => {
    vi.spyOn(apiHttp, 'get').mockRejectedValue(
      new ApiClientError('Failed to load library knowledge summary', 503),
    )

    const fallback = buildEmptyLibraryKnowledgeSummary('library-1')
    fallback.readinessSummary.documentCountsByReadiness.graphReady = 4
    fallback.graphCoverage.graphReadyDocumentCount = 4

    const projection = await resolveLibraryKnowledgeSummaryProjection('library-1', fallback)

    expect(projection.summary).toBe(fallback)
    expect(projection.warning).toBe(KNOWLEDGE_SUMMARY_UNAVAILABLE_WARNING)
  })
})
