import { act, renderHook, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { createElement, type ReactNode } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { Ai } from '@/shared/api/generated'
import { useAiAdminData } from './api'

const validModel = {
  id: 'model-alpha',
  providerCatalogId: 'provider-alpha',
  modelName: 'alpha-chat',
  lifecycleState: 'active',
  allowedBindingPurposes: ['query_answer'],
  availabilityState: 'available',
  availableAccountIds: [],
  pricing: {
    currencyCode: 'USD',
    inputPricePer1m: '0.20',
    cachedInputPricePer1m: '0.10',
    cacheWriteInputPricePer1m: '0.20',
    outputPricePer1m: '1.20',
  },
}

function queryClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false },
    },
  })
}

function wrapper(client: QueryClient) {
  return function QueryWrapper({ children }: { children: ReactNode }) {
    return createElement(QueryClientProvider, { client }, children)
  }
}

afterEach(() => {
  vi.restoreAllMocks()
})

describe('AI model catalog response boundary', () => {
  it('reports missing pricing as a query error and recovers through the shared retry', async () => {
    const client = queryClient()
    const listModels = vi
      .spyOn(Ai, 'listAiModels')
      .mockResolvedValueOnce({
        data: [{ ...validModel, pricing: undefined }],
      } as never)
      .mockResolvedValue({ data: [validModel] } as never)
    vi.spyOn(Ai, 'listAiProviders').mockResolvedValue({ data: [] } as never)
    vi.spyOn(Ai, 'listAiAccounts').mockResolvedValue({ data: [] } as never)
    vi.spyOn(Ai, 'listAiLibraryBindings').mockResolvedValue({ data: [] } as never)

    const { result } = renderHook(() => useAiAdminData('instance', {}), {
      wrapper: wrapper(client),
    })

    await waitFor(() =>
      expect(result.current.error).toEqual(
        new TypeError('Model catalog entry model-alpha.pricing must be an object'),
      ),
    )
    expect(result.current.models).toEqual([])
    expect(listModels).toHaveBeenCalledTimes(1)

    await act(async () => {
      await result.current.refetch()
    })

    await waitFor(() => expect(result.current.error).toBeNull())
    expect(result.current.models).toHaveLength(1)
    expect(listModels).toHaveBeenCalledTimes(2)
    client.clear()
  })
})
