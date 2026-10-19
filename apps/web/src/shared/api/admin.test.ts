import { afterEach, describe, expect, it, vi } from 'vitest'

import { adminApi, parseModelCatalogResponse, parseProviderCatalogResponse } from './admin'
import { Ai, Catalog } from './generated'
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

  it('removes undefined optional fields before creating a provider', async () => {
    const createAiProvider = vi.spyOn(Ai, 'createAiProvider').mockResolvedValue({
      data: { id: 'provider-1' },
    } as never)

    await adminApi.createProvider({
      kind: 'openai_compatible',
      displayName: 'Provider Beta',
      baseUrl: undefined,
    } as never)

    expect(createAiProvider).toHaveBeenCalledWith({
      body: { kind: 'openai_compatible', displayName: 'Provider Beta' },
    })
  })

  it('validates model catalog response shapes before exposing them to the UI', () => {
    expect(() => parseModelCatalogResponse({})).toThrow('Model catalog response must be an array')
    expect(() =>
      parseModelCatalogResponse([
        { id: 'model-1', availabilityState: 'invalid', availableAccountIds: [] },
      ]),
    ).toThrow('Model catalog entry model-1 has invalid availabilityState')
    expect(() =>
      parseModelCatalogResponse([
        { id: 'model-1', availabilityState: 'available', availableAccountIds: {} },
      ]),
    ).toThrow('Model catalog entry model-1 has invalid availableAccountIds')
  })

  it('rejects model catalog entries without the required pricing object', () => {
    expect(() =>
      parseModelCatalogResponse([
        {
          id: 'model-1',
          availabilityState: 'available',
          availableAccountIds: [],
        },
      ]),
    ).toThrow('Model catalog entry model-1.pricing must be an object')
  })

  it.each([
    'currencyCode',
    'inputPricePer1m',
    'cachedInputPricePer1m',
    'cacheWriteInputPricePer1m',
    'outputPricePer1m',
  ])('rejects model pricing without required field %s', (fieldName) => {
    const pricing: Record<string, unknown> = {
      currencyCode: 'USD',
      inputPricePer1m: '0.20',
      cachedInputPricePer1m: '0.10',
      cacheWriteInputPricePer1m: '0.20',
      outputPricePer1m: '1.20',
    }
    delete pricing[fieldName]

    expect(() =>
      parseModelCatalogResponse([
        {
          id: 'model-1',
          availabilityState: 'available',
          availableAccountIds: [],
          pricing,
        },
      ]),
    ).toThrow(`Model catalog entry model-1.pricing.${fieldName}`)
  })

  it('rejects malformed long-context pricing before model edit forms can consume it', () => {
    const model = {
      id: 'model-1',
      availabilityState: 'available',
      availableAccountIds: [],
      pricing: {
        currencyCode: 'USD',
        inputPricePer1m: '0.20',
        cachedInputPricePer1m: '0.10',
        cacheWriteInputPricePer1m: '0.20',
        outputPricePer1m: '1.20',
        longContext: {
          thresholdTokens: 0,
          inputPricePer1m: '0.40',
          cachedInputPricePer1m: '0.20',
          outputPricePer1m: '2.40',
        },
      },
    }

    expect(() => parseModelCatalogResponse([model])).toThrow(
      'Model catalog entry model-1.pricing.longContext.thresholdTokens must be a positive integer',
    )

    model.pricing.longContext.thresholdTokens = 128_000
    delete (model.pricing.longContext as Partial<typeof model.pricing.longContext>).outputPricePer1m
    expect(() => parseModelCatalogResponse([model])).toThrow(
      'Model catalog entry model-1.pricing.longContext.outputPricePer1m must be a non-negative decimal string',
    )
  })

  it('validates provider capabilities', () => {
    expect(() => parseProviderCatalogResponse([{ id: 'provider-1' }])).toThrow(
      'credentialPolicy must be an object',
    )
  })
})
