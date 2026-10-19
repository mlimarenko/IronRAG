import { act, renderHook, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { createElement, type ReactNode } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { Admin, Catalog, Ops, type CatalogLibraryResponse } from '@/shared/api/generated'
import { adminLibraryFacetsInfiniteOptions } from '@/shared/api/libraries'
import { librarySnapshotApi, queries } from '@/shared/api'
import { adaptLibrarySettings } from '../model/librariesAdapter'
import i18n from '@/shared/i18n'
import { formatCost } from '@/shared/format'
import {
  useLibraryFacets,
  useLibraryAdminMutations,
  type LibraryFacetScope,
} from '@/features/admin/libraries/api/useLibrariesAdmin'

const t = i18n.t.bind(i18n)

const scope: LibraryFacetScope = {
  search: '',
  normalizedSearch: null,
  hints: null,
  filters: undefined,
}

const options = {
  enabled: true,
  locale: 'en' as const,
  scope,
  t,
}

const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } })
const wrapper = ({ children }: Readonly<{ children: ReactNode }>) =>
  createElement(QueryClientProvider, { client: queryClient }, children)

function response(column: string) {
  return { data: { column, values: [], nextCursor: null } }
}

function partialLabel(amount: string, currencyCode: string, unpricedCallCount: number): string {
  return formatCost(
    {
      costRollupFreshness: 'current',
      totalCost: amount,
      currencyCode,
      providerCallCount: 6,
      unpricedProviderCallCount: unpricedCallCount,
      costCoverage: 'partial',
    },
    i18n.t('common.costUnknown'),
    i18n.t('documents.unpriced'),
    'en',
  )
}

afterEach(() => {
  queryClient.clear()
  vi.restoreAllMocks()
})

describe('server library facets carry column-local typed hints', () => {
  it('serializes the shared grid NULL label as an unavailable cost bucket', async () => {
    const facets = vi
      .spyOn(Admin, 'listAdminLibraryFacets')
      .mockResolvedValue(response('cost') as never)
    const { result } = renderHook(() => useLibraryFacets(options), { wrapper })
    act(() => result.current.serverFacets.onOpen('cost'))
    act(() => result.current.serverFacets.onSearch('cost', t('dataGrid.nullValue')))
    await waitFor(() => {
      const query = facets.mock.calls.at(-1)?.[0]?.query
      expect(query?.facetSearch).toBe(t('dataGrid.nullValue'))
      expect(JSON.parse(query?.facetSearchHints ?? 'null')).toEqual({ unavailable: true })
    })
  })
  it('serializes the cost term into the facet search and its typed hints', async () => {
    const facets = vi
      .spyOn(Admin, 'listAdminLibraryFacets')
      .mockResolvedValue(response('cost') as never)
    const { result } = renderHook(() => useLibraryFacets(options), { wrapper })
    act(() => result.current.serverFacets.onOpen('cost'))
    const label = partialLabel('1234.56', 'USD', 3)
    act(() => result.current.serverFacets.onSearch('cost', label))
    await waitFor(
      () => {
        const lastQuery = facets.mock.calls.at(-1)?.[0]?.query as Record<string, unknown>
        expect(lastQuery.facetSearch).toBe(label)
      },
      { timeout: 2000 },
    )
    const lastQuery = facets.mock.calls.at(-1)?.[0]?.query as Record<string, unknown>
    expect(lastQuery.facetColumn).toBe('cost')
    expect(JSON.parse(String(lastQuery.facetSearchHints))).toEqual({
      costs: [
        {
          amount: '1234.56',
          currencyCode: 'USD',
          coverage: 'partial',
          unpricedCallCount: 3,
        },
      ],
    })
  })

  it('never forwards the cost label hints into a text-only column', async () => {
    const facets = vi
      .spyOn(Admin, 'listAdminLibraryFacets')
      .mockResolvedValue(response('library') as never)
    const { result } = renderHook(() => useLibraryFacets(options), { wrapper })
    act(() => result.current.serverFacets.onOpen('library'))
    act(() => result.current.serverFacets.onSearch('library', 'Cost unknown'))
    await waitFor(() =>
      expect(facets).toHaveBeenLastCalledWith(
        expect.objectContaining({
          query: expect.objectContaining({ facetColumn: 'library', facetSearch: 'Cost unknown' }),
        }),
      ),
    )
    const lastQuery = facets.mock.calls.at(-1)?.[0]?.query as Record<string, unknown>
    expect(lastQuery.facetSearchHints).toBeUndefined()
  })
})

const library: CatalogLibraryResponse = {
  id: 'library',
  workspaceId: 'workspace',
  displayName: 'A',
  slug: 'library',
  description: null,
  extractionPrompt: null,
  lifecycleState: 'active',
  includeDocumentHintInMcpAnswers: false,
  ingestionReadiness: { ready: true, missingBindingPurposes: [] },
  runtimeReadiness: { missingBindingPurposes: [] },
  recognitionPolicy: { rasterImageEngine: 'vision' },
  retrievalConfig: { lexical: { textSearchConfig: 'english' } },
  webIngestPolicy: {
    crawlFilter: { allowPatterns: [], blockPatterns: [] },
    materializationFilter: { allowPatterns: [], blockPatterns: [] },
  },
}

describe('library mutation facet cache', () => {
  it('waits for a late rename before invalidating an early partial-save failure', async () => {
    let name = 'A'
    let finishRename!: () => void
    const renameGate = new Promise<void>((resolve) => {
      finishRename = resolve
    })
    const facets = vi.spyOn(Admin, 'listAdminLibraryFacets').mockImplementation(
      async ({ query }) =>
        ({
          data: { column: query.facetColumn, values: [{ text: name, count: 1 }], nextCursor: null },
        }) as never,
    )
    vi.spyOn(Catalog, 'updateCatalogLibrary').mockImplementation(async () => {
      await renameGate
      name = 'B'
      return { data: library } as never
    })
    vi.spyOn(Catalog, 'updateCatalogLibraryRecognitionPolicy').mockResolvedValue({
      data: {},
    } as never)
    vi.spyOn(Catalog, 'updateCatalogLibraryWebIngestPolicy').mockResolvedValue({
      data: {},
    } as never)
    const retrieval = vi
      .spyOn(Catalog, 'updateCatalogLibraryRetrievalConfig')
      .mockRejectedValue(new Error('early retrieval failure'))
    const { result } = renderHook(
      () => ({
        ...useLibraryFacets(options),
        mutations: useLibraryAdminMutations(),
      }),
      { wrapper },
    )
    act(() => result.current.serverFacets.onOpen('library'))
    await waitFor(() =>
      expect(
        result.current.serverFacets.page('library')?.options.map((option) => option.label),
      ).toEqual(['A']),
    )
    let error: unknown
    let mutation!: Promise<void>
    await act(async () => {
      mutation = result.current.mutations.saveSettings
        .mutateAsync({
          libraryId: library.id,
          current: library,
          draft: { ...adaptLibrarySettings(library, library.retrievalConfig), displayName: 'B' },
        })
        .catch((failure: unknown) => {
          error = failure
        })
      await Promise.resolve()
    })
    expect(retrieval).toHaveBeenCalledOnce()
    // The failed write does not make the still-running rename safe to refetch.
    const beforeLateWrite = { error, requests: facets.mock.calls.length }
    await act(async () => {
      finishRename()
      await mutation
    })
    expect(beforeLateWrite.error).toBeUndefined()
    expect(beforeLateWrite.requests).toBe(1)
    expect(error).toBeInstanceOf(Error)
    act(() => result.current.serverFacets.onOpen('library'))
    await waitFor(() =>
      expect(
        result.current.serverFacets.page('library')?.options.map((option) => option.label),
      ).toEqual(['B']),
    )
  })
  it.each(['delete', 'library restore', 'workspace restore'] as const)(
    'invalidates inactive facet pages after %s settles',
    async (mutation) => {
      vi.spyOn(Admin, 'listAdminLibraryFacets').mockResolvedValue(response('cost') as never)
      vi.spyOn(Catalog, 'deleteCatalogLibrary').mockResolvedValue({
        data: { operationId: 'operation' },
      } as never)
      vi.spyOn(Catalog, 'importCatalogWorkspaceSnapshot').mockResolvedValue({
        data: { operationId: 'operation' },
      } as never)
      vi.spyOn(librarySnapshotApi, 'import').mockResolvedValue({
        operationId: 'operation',
      } as never)
      vi.spyOn(Ops, 'getAsyncOperation').mockResolvedValue({ data: { status: 'ready' } } as never)
      const facets = adminLibraryFacetsInfiniteOptions({ column: 'cost' })
      await queryClient.fetchInfiniteQuery(facets)
      expect(queryClient.getQueryState(facets.queryKey)?.isInvalidated).toBe(false)
      const { result } = renderHook(useLibraryAdminMutations, { wrapper })
      if (mutation === 'delete') {
        await result.current.deleteLibrary.mutateAsync({
          libraryId: 'library',
          workspaceId: 'workspace',
        })
      } else {
        await result.current.restoreSnapshot.mutateAsync({
          scope: mutation === 'library restore' ? 'library' : 'workspace',
          libraryId: 'library',
          workspaceId: 'workspace',
          file: new File(['snapshot'], 'snapshot.tar.zst'),
          overwrite: 'replace',
        })
      }
      expect(queryClient.getQueryState(facets.queryKey)?.isInvalidated).toBe(true)
    },
  )
  it.each([false, true])(
    'refreshes a closed menu after rename (partial failure: %s)',
    async (fail) => {
      let name = 'A'
      vi.spyOn(Admin, 'listAdminLibraryFacets').mockImplementation(
        async ({ query }) =>
          ({
            data: {
              column: query.facetColumn,
              values: [{ text: name, count: 1 }],
              nextCursor: null,
            },
          }) as never,
      )
      vi.spyOn(Catalog, 'updateCatalogLibrary').mockImplementation(async () => {
        name = 'B'
        return { data: library } as never
      })
      vi.spyOn(Catalog, 'updateCatalogLibraryRecognitionPolicy').mockResolvedValue({
        data: {},
      } as never)
      vi.spyOn(Catalog, 'updateCatalogLibraryWebIngestPolicy').mockResolvedValue({
        data: {},
      } as never)
      const retrieval = vi.spyOn(Catalog, 'updateCatalogLibraryRetrievalConfig')
      if (fail) retrieval.mockRejectedValue(new Error('retrieval update failed'))
      else retrieval.mockResolvedValue({ data: {} } as never)

      const inactive = adminLibraryFacetsInfiniteOptions({ column: 'workspace', facetLimit: 50 })
      await queryClient.fetchInfiniteQuery(inactive)
      const listKey = queries.listAdminLibrariesQueryKey()
      const detailKey = queries.getCatalogLibraryQueryKey({ path: { libraryId: 'library' } })
      queryClient.setQueryData(listKey, {})
      queryClient.setQueryData(detailKey, library)
      const { result } = renderHook(
        () => ({
          ...useLibraryFacets(options),
          mutations: useLibraryAdminMutations(),
        }),
        { wrapper },
      )
      act(() => result.current.serverFacets.onOpen('library'))
      await waitFor(() =>
        expect(
          result.current.serverFacets.page('library')?.options.map((option) => option.label),
        ).toEqual(['A']),
      )
      // Closing the shared menu leaves this observer enabled on the same key.
      const input = {
        libraryId: library.id,
        current: library,
        draft: { ...adaptLibrarySettings(library, library.retrievalConfig), displayName: 'B' },
      }
      await act(async () => {
        const mutation = result.current.mutations.saveSettings.mutateAsync(input)
        if (fail) await expect(mutation).rejects.toThrow('retrieval update failed')
        else await mutation
      })
      act(() => result.current.serverFacets.onOpen('library'))
      await waitFor(() =>
        expect(
          result.current.serverFacets.page('library')?.options.map((option) => option.label),
        ).toEqual(['B']),
      )
      expect(queryClient.getQueryState(inactive.queryKey)?.isInvalidated).toBe(true)
      expect(queryClient.getQueryState(listKey)?.isInvalidated).toBe(true)
      expect(queryClient.getQueryState(detailKey)?.isInvalidated).toBe(true)
      const reopened = await queryClient.fetchInfiniteQuery(inactive)
      expect(reopened.pages[0]?.values.map((value) => value.text)).toEqual(['B'])
    },
  )
})
