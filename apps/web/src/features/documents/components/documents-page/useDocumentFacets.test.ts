import { act, renderHook, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { createElement, type ReactNode } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { documentsApi } from '@/shared/api'
import i18n from '@/shared/i18n'
import { useDocumentFacets, type DocumentFacetScope } from './useDocumentFacets'

const initialScope: DocumentFacetScope = {
  columnFilters: undefined,
  search: '',
  searchStatus: [],
  normalizedSearch: null,
  searchRanges: null,
  status: undefined,
  indexing: undefined,
  ids: undefined,
  snapshotAt: '2026-10-01T12:00:00Z',
}

const options = {
  libraryId: 'library-one',
  enabled: true,
  locale: 'en' as const,
  t: i18n.t,
  onSnapshot: vi.fn(),
}

const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } })
const wrapper = ({ children }: Readonly<{ children: ReactNode }>) =>
  createElement(QueryClientProvider, { client: queryClient }, children)

function response() {
  return {
    column: 'file_type' as const,
    snapshotAt: initialScope.snapshotAt!,
    nextCursor: 'next',
    values: [
      { value: 'application/pdf', count: 2 },
      { value: 'text/plain', count: 3 },
    ],
    costRollupFreshness: 'current' as const,
    scopePending: false,
  }
}

afterEach(() => {
  queryClient.clear()
  vi.restoreAllMocks()
  vi.useRealTimers()
})

describe('server document facets', () => {
  it('keeps a metadata facet available while only its rollup reconciles', async () => {
    vi.spyOn(documentsApi, 'facets').mockResolvedValue({
      ...response(),
      costRollupFreshness: 'reconciling',
    })
    const { result } = renderHook(() => useDocumentFacets({ ...options, scope: initialScope }), {
      wrapper,
    })
    act(() => result.current.serverFacets.onOpen('file_type'))
    await waitFor(() => expect(result.current.serverFacets.page('file_type')?.pending).toBe(false))
    expect(result.current.serverFacets.page('file_type')?.options).toHaveLength(2)
    expect(result.current.serverFacets.page('file_type')?.hasMore).toBe(true)
  })

  it('does not turn a pending facet into a resolved empty domain', async () => {
    vi.spyOn(documentsApi, 'facets').mockResolvedValue({
      ...response(),
      values: [],
      nextCursor: null,
      costRollupFreshness: 'reconciling',
      scopePending: true,
    })
    const { result } = renderHook(() => useDocumentFacets({ ...options, scope: initialScope }), {
      wrapper,
    })
    act(() => result.current.serverFacets.onOpen('file_type'))
    await waitFor(() => expect(queryClient.isFetching()).toBe(0))
    await waitFor(() => expect(result.current.serverFacets.page('file_type')?.pending).toBe(true))
    expect(result.current.serverFacets.page('file_type')?.options).toHaveLength(0)
    expect(result.current.serverFacets.page('file_type')?.hasMore).toBe(false)
  })
  it.each([
    ['file_size', '1.5 KB', { facetSearchRanges: { fileSize: { min: 1485, max: 1588 } } }],
    [
      'pipeline_time',
      '1m 30s',
      { facetSearchRanges: { pipelineTime: { min: 90_000, max: 91_000 } } },
    ],
    ['status', 'Ready', { facetSearchStatus: ['ready'] }],
    ['cost', '$0.10', { facetSearch: '0.10' }],
  ] as const)(
    'searches displayed %s values without changing the main scope',
    async (column, search, hints) => {
      const facets = vi.spyOn(documentsApi, 'facets').mockResolvedValue(response())
      const { result } = renderHook(
        () =>
          useDocumentFacets({
            ...options,
            scope: { ...initialScope, search: 'full-scope', searchStatus: ['failed'] },
          }),
        { wrapper },
      )
      act(() => result.current.serverFacets.onOpen(column))
      act(() => result.current.serverFacets.onSearch(column, search))
      await waitFor(() =>
        expect(facets).toHaveBeenLastCalledWith(
          expect.objectContaining({
            facetColumn: column,
            search: 'full-scope',
            searchStatus: ['failed'],
            ...hints,
          }),
          expect.any(AbortSignal),
        ),
      )
    },
  )
  it('isolates a late response from an old authorized scope and aborts its request', async () => {
    let finishOld!: (value: ReturnType<typeof response>) => void
    const facets = vi
      .spyOn(documentsApi, 'facets')
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            finishOld = resolve
          }),
      )
      .mockResolvedValue({ ...response(), values: [{ value: 'new-scope', count: 1 }] })
    const { result, rerender } = renderHook(
      ({ scope }) => useDocumentFacets({ ...options, scope }),
      { initialProps: { scope: initialScope }, wrapper },
    )
    act(() => result.current.serverFacets.onOpen('file_type'))
    await waitFor(() => expect(facets).toHaveBeenCalledTimes(1))
    const oldSignal = facets.mock.calls[0]![1]!
    rerender({ scope: { ...initialScope, search: 'new' } })
    await waitFor(() =>
      expect(result.current.serverFacets.page('file_type')?.options[0]?.value).toBe('new-scope'),
    )
    expect(oldSignal.aborted).toBe(true)
    await act(async () => finishOld(response()))
    expect(result.current.serverFacets.page('file_type')?.options[0]?.value).toBe('new-scope')
  })

  it('pins the second facet page to the first response snapshot', async () => {
    const facets = vi.spyOn(documentsApi, 'facets').mockResolvedValue(response())
    const { result } = renderHook(
      () =>
        useDocumentFacets({
          ...options,
          scope: { ...initialScope, snapshotAt: null },
        }),
      { wrapper },
    )
    act(() => result.current.serverFacets.onOpen('file_type'))
    await waitFor(() => expect(result.current.serverFacets.page('file_type')?.pending).toBe(false))
    await act(async () => result.current.serverFacets.onLoadMore('file_type'))
    expect(facets).toHaveBeenLastCalledWith(
      expect.objectContaining({
        facetCursor: 'next',
        snapshotAt: initialScope.snapshotAt,
      }),
      expect.any(AbortSignal),
    )
  })
  it('reloads the mounted menu after a selection and retains alternative OR values', async () => {
    const facets = vi.spyOn(documentsApi, 'facets').mockResolvedValue(response())
    const { result, rerender } = renderHook(
      ({ scope }) => useDocumentFacets({ ...options, scope }),
      { initialProps: { scope: initialScope }, wrapper },
    )
    act(() => result.current.serverFacets.onOpen('file_type'))
    await waitFor(() => expect(result.current.serverFacets.page('file_type')?.pending).toBe(false))
    rerender({ scope: { ...initialScope, columnFilters: { fileType: ['application/pdf'] } } })
    await waitFor(() => expect(facets).toHaveBeenCalledTimes(2))
    await waitFor(() =>
      expect(
        result.current.serverFacets.page('file_type')?.options.map((option) => option.value),
      ).toEqual(['application/pdf', 'text/plain']),
    )
    expect(facets).toHaveBeenLastCalledWith(
      expect.objectContaining({
        columnFilters: { fileType: ['application/pdf'] },
        snapshotAt: initialScope.snapshotAt,
      }),
      expect.any(AbortSignal),
    )
  })

  it('sends the current search term and retains it when loading the next value page', async () => {
    const facets = vi.spyOn(documentsApi, 'facets').mockResolvedValue(response())
    const { result } = renderHook(() => useDocumentFacets({ ...options, scope: initialScope }), {
      wrapper,
    })
    await act(async () => result.current.serverFacets.onOpen('file_type'))
    act(() => result.current.serverFacets.onSearch('file_type', 'pdf'))
    await waitFor(() =>
      expect(facets).toHaveBeenLastCalledWith(
        expect.objectContaining({ facetSearch: 'pdf' }),
        expect.any(AbortSignal),
      ),
    )
    await waitFor(() => expect(result.current.serverFacets.page('file_type')?.pending).toBe(false))
    await act(async () => result.current.serverFacets.onLoadMore('file_type'))
    await waitFor(() =>
      expect(facets).toHaveBeenLastCalledWith(
        expect.objectContaining({ facetSearch: 'pdf', facetCursor: 'next' }),
        expect.any(AbortSignal),
      ),
    )
  })

  it('keeps a reconciling cost facet pending, not a finished empty list, retaining selected chips', async () => {
    const facets = vi.spyOn(documentsApi, 'facets').mockResolvedValue({
      ...response(),
      column: 'cost' as const,
      values: [],
      nextCursor: null,
      costRollupFreshness: 'reconciling' as const,
      scopePending: true,
    })
    const { result } = renderHook(
      () =>
        useDocumentFacets({
          ...options,
          scope: { ...initialScope, columnFilters: { cost: ['0.50'] } },
        }),
      { wrapper },
    )
    act(() => result.current.serverFacets.onOpen('cost'))
    await waitFor(() => expect(queryClient.isFetching()).toBe(0))
    await waitFor(() => expect(result.current.serverFacets.page('cost')?.pending).toBe(true))
    // A reconciling facet is pending with no continuation, so the menu keeps
    // its selected chip (added by the grid menu) and shows "loading" instead of
    // claiming the cost facet has no values.
    expect(result.current.serverFacets.page('cost')?.options).toEqual([])
    expect(result.current.serverFacets.page('cost')?.hasMore).toBe(false)
    expect(facets).toHaveBeenCalled()
  })
})
