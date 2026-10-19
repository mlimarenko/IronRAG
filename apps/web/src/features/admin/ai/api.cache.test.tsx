import { act, renderHook, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { createElement, type ReactNode } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { adminApi, queries } from '@/shared/api'
import { Ai, type AiAccountResponse } from '@/shared/api/generated'
import { useAccountDetail, useAiAdminMutations } from './api'

const accountId = '00000000-0000-4000-8000-000000000001'

function rawAccount(label: string): AiAccountResponse {
  return {
    apiKeySummary: 'configured',
    createdAt: '2026-01-01T00:00:00Z',
    credentialState: 'active',
    id: accountId,
    label,
    libraryId: '00000000-0000-4000-8000-000000000002',
    providerCatalogId: '00000000-0000-4000-8000-000000000003',
    scopeKind: 'library',
    updatedAt: '2026-01-02T00:00:00Z',
    workspaceId: '00000000-0000-4000-8000-000000000004',
  }
}

function queryClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: 30_000 },
      mutations: { retry: false },
    },
  })
}

function wrapper(client: QueryClient) {
  return function QueryWrapper({ children }: { children: ReactNode }) {
    return createElement(QueryClientProvider, { client }, children)
  }
}

const operationKey = (operationId: string) => [{ _id: operationId }] as const

function seedOperation(client: QueryClient, operationId: string) {
  const key = operationKey(operationId)
  client.setQueryData(key, { seeded: operationId })
  return key
}

afterEach(() => {
  vi.restoreAllMocks()
})

describe('AI account detail cache fencing', () => {
  it('refetches a fresh account detail before treating a cached value as authoritative', async () => {
    const client = queryClient()
    const cached = rawAccount('Cached label')
    const fresh = rawAccount('Fresh label')
    const options = queries.getAiAccountOptions({ path: { accountId } })
    client.setQueryData(options.queryKey, cached)

    let releaseResponse: (() => void) | undefined
    const responseGate = new Promise<void>((resolve) => {
      releaseResponse = resolve
    })
    const request = vi.fn()
    vi.spyOn(Ai, 'getAiAccount').mockImplementation(
      async () =>
        ({
          data: await (async () => {
            request()
            await responseGate
            return fresh
          })(),
        }) as never,
    )

    const { result } = renderHook(() => useAccountDetail(accountId, []), {
      wrapper: wrapper(client),
    })

    await waitFor(() => expect(request).toHaveBeenCalledTimes(1))
    expect(result.current.isFetching).toBe(true)
    expect(result.current.data?.label).toBe('Cached label')

    releaseResponse?.()
    await waitFor(() => expect(result.current.data?.label).toBe('Fresh label'))
    expect(result.current.isFetching).toBe(false)
    client.clear()
  })
})

describe('AI account mutation invalidation', () => {
  it('invalidates account lists, account details, and model availability after update', async () => {
    const client = queryClient()
    const listKey = seedOperation(client, 'listAiAccounts')
    const detailKey = seedOperation(client, 'getAiAccount')
    const modelListKey = seedOperation(client, 'listAiModels')
    const modelDetailKey = seedOperation(client, 'getAiModel')
    vi.spyOn(adminApi, 'updateAccount').mockResolvedValue(rawAccount('Updated label'))
    const { result } = renderHook(() => useAiAdminMutations(), { wrapper: wrapper(client) })

    await act(() =>
      result.current.accountUpdate.mutateAsync({
        id: accountId,
        body: {
          label: 'Updated label',
          credentialState: 'active',
          expectedUpdatedAt: '2026-01-02T00:00:00Z',
        },
      }),
    )

    expect(client.getQueryState(listKey)?.isInvalidated).toBe(true)
    expect(client.getQueryState(detailKey)?.isInvalidated).toBe(true)
    expect(client.getQueryState(modelListKey)?.isInvalidated).toBe(true)
    expect(client.getQueryState(modelDetailKey)?.isInvalidated).toBe(true)
    client.clear()
  })

  it('invalidates account lists and model availability after create', async () => {
    const client = queryClient()
    const listKey = seedOperation(client, 'listAiAccounts')
    const modelListKey = seedOperation(client, 'listAiModels')
    const modelDetailKey = seedOperation(client, 'getAiModel')
    vi.spyOn(adminApi, 'createAccount').mockResolvedValue(rawAccount('Created label'))
    const { result } = renderHook(() => useAiAdminMutations(), { wrapper: wrapper(client) })

    await act(() =>
      result.current.accountCreate.mutateAsync({
        scopeKind: 'instance',
        providerCatalogId: '00000000-0000-4000-8000-000000000003',
        label: 'Created label',
        apiKey: 'synthetic-token',
      }),
    )

    expect(client.getQueryState(listKey)?.isInvalidated).toBe(true)
    expect(client.getQueryState(modelListKey)?.isInvalidated).toBe(true)
    expect(client.getQueryState(modelDetailKey)?.isInvalidated).toBe(true)
    client.clear()
  })

  it('invalidates account lists, account details, and model availability after delete', async () => {
    const client = queryClient()
    const listKey = seedOperation(client, 'listAiAccounts')
    const detailKey = seedOperation(client, 'getAiAccount')
    const modelListKey = seedOperation(client, 'listAiModels')
    const modelDetailKey = seedOperation(client, 'getAiModel')
    vi.spyOn(adminApi, 'deleteAccount').mockResolvedValue(undefined)
    const { result } = renderHook(() => useAiAdminMutations(), { wrapper: wrapper(client) })

    await act(() => result.current.accountDelete.mutateAsync(accountId))

    expect(client.getQueryState(listKey)?.isInvalidated).toBe(true)
    expect(client.getQueryState(detailKey)?.isInvalidated).toBe(true)
    expect(client.getQueryState(modelListKey)?.isInvalidated).toBe(true)
    expect(client.getQueryState(modelDetailKey)?.isInvalidated).toBe(true)
    client.clear()
  })
})
