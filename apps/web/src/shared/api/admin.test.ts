import { afterEach, describe, expect, it, vi } from 'vitest'

import { adminApi } from './admin'
import { Catalog } from './generated'
import type { CatalogLibraryResponse } from './generated'

const baseLibrary = {
  id: 'library-1',
  workspaceId: 'workspace-1',
  slug: 'library-one',
  displayName: 'Library One',
  description: 'Catalog description',
  extractionPrompt: 'Keep existing extraction prompt.',
  includeDocumentHintInMcpAnswers: false,
  ingestionReadiness: {
    ready: true,
    missingBindingPurposes: [],
  },
  lifecycleState: 'active',
  recognitionPolicy: {
    rasterImageEngine: 'disabled',
  },
  webIngestPolicy: {
    crawlFilter: {
      include: [],
      exclude: [],
    },
    materializationFilter: {
      include: [],
      exclude: [],
    },
  },
} as unknown as CatalogLibraryResponse

describe('adminApi', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('updates MCP document hints through the catalog library endpoint', async () => {
    const updatedLibrary = {
      ...baseLibrary,
      includeDocumentHintInMcpAnswers: true,
    }
    const getLibrary = vi
      .spyOn(Catalog, 'getCatalogLibrary')
      .mockResolvedValueOnce({ data: baseLibrary, error: undefined })
    const updateLibrary = vi
      .spyOn(Catalog, 'updateCatalogLibrary')
      .mockResolvedValueOnce({ data: updatedLibrary, error: undefined })

    await expect(
      adminApi.updateLibraryMcpSettings('library-1', {
        includeDocumentHintInMcpAnswers: true,
      }),
    ).resolves.toBe(updatedLibrary)

    expect(getLibrary).toHaveBeenCalledWith({ path: { libraryId: 'library-1' } })
    expect(updateLibrary).toHaveBeenCalledWith({
      path: { libraryId: 'library-1' },
      body: {
        slug: 'library-one',
        displayName: 'Library One',
        description: 'Catalog description',
        extractionPrompt: 'Keep existing extraction prompt.',
        lifecycleState: 'active',
        includeDocumentHintInMcpAnswers: true,
      },
    })
  })
})
