import { act, renderHook, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { createElement, type ReactNode } from 'react'
import { MemoryRouter } from 'react-router-dom'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { queries, type DocumentListPageResponse } from '@/shared/api'
import i18n from '@/shared/i18n'
import { useDocumentsPageUrlState } from './documentsPageState'
import { useDocumentsQueries } from './useDocumentsQueries'

afterEach(() => vi.restoreAllMocks())

const snapshot = '2026-10-01T12:00:00Z'

function mount(
  controller: () => DocumentListPageResponse,
  sortKey: 'cost' | 'uploaded_at' = 'cost',
) {
  const listOptions = queries.listContentDocumentsOptions
  const listSpy = vi
    .spyOn(queries, 'listContentDocumentsOptions')
    .mockImplementation((options) => ({
      ...listOptions(options),
      queryFn: async () => controller(),
    }))
  // Only the list endpoint is exercised; the other queries the hook mounts
  // answer an empty body so they never throw.
  vi.spyOn(globalThis, 'fetch').mockImplementation(async () => Response.json({ items: [] }))
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  const wrapper = ({ children }: Readonly<{ children: ReactNode }>) =>
    createElement(QueryClientProvider, { client }, createElement(MemoryRouter, null, children))
  const hook = renderHook(
    () => {
      const url = useDocumentsPageUrlState({
        libraryId: 'library-one',
        tableState: {
          pageSize: 50,
          sort: { key: sortKey, direction: 'desc' },
          showDetailColumns: true,
        },
        setTableState: vi.fn(),
      })
      const documents = useDocumentsQueries({
        activeLibrary: {
          id: 'library-one',
          workspaceId: 'workspace',
          name: 'library-one',
          createdAt: snapshot,
          includeDocumentHintInMcpAnswers: false,
          ingestionReady: true,
          queryReady: true,
          missingBindingPurposes: [],
        },
        activeWorkspace: null,
        locale: 'en',
        ...url,
        t: i18n.t,
      })
      return documents
    },
    { wrapper },
  )
  return { ...hook, client, listSpy }
}

const reconciling: DocumentListPageResponse = {
  items: [],
  snapshotAt: snapshot,
  totalCount: null,
  scopePending: true,
  costRollupFreshness: 'reconciling',
}

const currentEmpty: DocumentListPageResponse = {
  items: [],
  snapshotAt: snapshot,
  totalCount: 0,
  scopePending: false,
  costRollupFreshness: 'current',
}

describe('cost-dependent list pending vs empty', () => {
  it('restarts a pending cost cursor chain without resetting the pinned clock', async () => {
    let page: DocumentListPageResponse = {
      ...currentEmpty,
      totalCount: 100,
      nextCursor: 'page-two',
    }
    const { result, client, listSpy } = mount(() => page)
    await waitFor(() => expect(result.current.pagination.canGoNext).toBe(true))
    act(() => result.current.pagination.goToNextPage())
    await waitFor(() => expect(result.current.pagination.currentPageNumber).toBe(2))
    page = reconciling
    await act(async () => {
      await client.invalidateQueries()
    })
    await waitFor(() => expect(result.current.pagination.currentPageNumber).toBe(1))
    expect(result.current.isScopePending).toBe(true)
    expect(result.current.snapshotAt).toBe(snapshot)
    page = currentEmpty
    await act(async () => {
      await client.invalidateQueries()
    })
    await waitFor(() => expect(result.current.isScopePending).toBe(false))
    expect(result.current.pagination.currentPageNumber).toBe(1)
    const lastPageQuery = listSpy.mock.calls
      .filter(([options]) => options.query?.limit === 50)
      .at(-1)![0].query!
    expect(lastPageQuery.cursor).toBeUndefined()
    expect(lastPageQuery.snapshotAt).toBe(snapshot)
  })
  it('keeps metadata scope counts available while only the cost rollup reconciles', async () => {
    const { result } = mount(
      () => ({ ...reconciling, scopePending: false, totalCount: 7 }),
      'uploaded_at',
    )
    await waitFor(() => expect(result.current.costReconciling).toBe(true))
    expect(result.current.isScopePending).toBe(false)
    expect(result.current.filteredTotal).toBe(7)
  })
  it('marks a reconciling cost sort pending instead of claiming no documents match', async () => {
    const { result } = mount(() => reconciling)
    await waitFor(() => expect(result.current.costReconciling).toBe(true))
    expect(result.current.isScopePending).toBe(true)
  })

  it('treats a current empty cost page as a real empty result', async () => {
    const { result } = mount(() => currentEmpty)
    await waitFor(() => expect(result.current.costReconciling).toBe(false))
    expect(result.current.isScopePending).toBe(false)
    expect(result.current.items).toHaveLength(0)
  })
})
