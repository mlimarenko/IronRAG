import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, renderHook } from '@testing-library/react'
import type { ReactNode } from 'react'
import { createElement } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { DocumentItem, Library } from '@/shared/types'

const mocks = vi.hoisted(() => ({
  batchCancel: vi.fn(),
  batchDelete: vi.fn(),
  batchReprocess: vi.fn(),
  list: vi.fn(),
  reprocess: vi.fn(),
}))

// The controller's only network boundary is `documentsApi`; the bulk-operation
// query stays inert because these tests never start one.
vi.mock('@/shared/api', () => ({
  ASYNC_OPERATION_TERMINAL_STATES: new Set(['ready', 'failed', 'canceled']),
  documentsApi: {
    batchCancel: mocks.batchCancel,
    batchDelete: mocks.batchDelete,
    batchReprocess: mocks.batchReprocess,
    list: mocks.list,
    reprocess: mocks.reprocess,
  },
  queries: {
    getAsyncOperationOptions: (options: { path: { operationId: string } }) => ({
      queryKey: ['async-operation', options.path.operationId],
      queryFn: () => Promise.reject(new Error('not exercised by this test')),
    }),
  },
}))

vi.mock('sonner', () => ({
  toast: {
    error: vi.fn(),
    success: vi.fn(),
    warning: vi.fn(),
  },
}))

const { useDocumentsListController } = await import('./useDocumentsListController')

const library: Library = {
  id: 'library-1',
  workspaceId: 'workspace-1',
  name: 'Handbooks',
  createdAt: '2026-08-01T00:00:00Z',
  includeDocumentHintInMcpAnswers: false,
  ingestionReady: true,
  queryReady: true,
  missingBindingPurposes: [],
}

function documentItem(id: string): DocumentItem {
  return {
    id,
    documentState: 'active',
    fileName: `${id}.pdf`,
    fileType: 'pdf',
    fileSize: 1024,
    uploadedAt: '2026-08-20T09:00:00Z',
    cost: null,
    status: 'ready',
    readiness: 'graph_ready',
  }
}

function listPage(items: DocumentItem[], nextCursor: string | null, snapshotAt: string) {
  return {
    costRollupFreshness: 'current',
    items,
    nextCursor,
    scopePending: false,
    snapshotAt,
  }
}

type ControllerInput = Parameters<typeof useDocumentsListController>[0]

const input: ControllerInput = {
  activeLibrary: library,
  activateListPollGrace: vi.fn(),
  errorMessage: (error: unknown, fallback: string) =>
    error instanceof Error ? error.message : fallback,
  filteredTotal: null,
  hasMorePages: true,
  items: [],
  loadFirstPage: vi.fn(async () => undefined),
  pageSize: 25,
  setTableState: vi.fn(),
  sortBy: 'file_name',
  sortOrder: 'asc',
  sortValue: 'file_name:asc',
  listFilter: { status: ['ready'] },
  columnFilters: { status: null },
  search: 'invoice',
  searchStatus: ['failed'],
  normalizedSearch: 'invoice',
  searchRanges: null,
  snapshotAt: null,
  t: ((key: string) => key) as never,
  updateSearchParamState: vi.fn(),
}

function renderController(overrides: Partial<ControllerInput> = {}) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const props: ControllerInput = { ...input, ...overrides }
  function Wrapper({ children }: Readonly<{ children: ReactNode }>) {
    return createElement(QueryClientProvider, { client: queryClient }, children)
  }
  const rendered = renderHook(() => useDocumentsListController(props), { wrapper: Wrapper })
  return { ...rendered, queryClient }
}

function selected(result: { current: { selectedIds: Set<string> } }): string[] {
  return [...result.current.selectedIds].sort()
}

describe('useDocumentsListController selection scope', () => {
  beforeEach(() => {
    mocks.list.mockReset()
  })

  it('selects every matching page by walking the same filter, search and sort scope', async () => {
    mocks.list
      .mockResolvedValueOnce(
        listPage(
          [documentItem('doc-a'), documentItem('doc-b')],
          'cursor-2',
          '2026-11-18T00:00:00Z',
        ),
      )
      .mockResolvedValueOnce(listPage([documentItem('doc-c')], null, '2026-11-18T00:05:00Z'))

    const { result, unmount, queryClient } = renderController()
    try {
      await act(async () => {
        await result.current.selectAllMatching()
      })

      expect(mocks.list).toHaveBeenCalledTimes(2)
      // Absent scopes stay absent on the wire instead of travelling as empty
      // values, and the walk counts no total it does not display.
      expect(mocks.list).toHaveBeenNthCalledWith(1, {
        libraryId: 'library-1',
        limit: 25,
        sortBy: 'file_name',
        sortOrder: 'asc',
        includeTotal: false,
        search: 'invoice',
        searchStatus: ['failed'],
        normalizedSearch: 'invoice',
        columnFilters: { status: null },
        status: ['ready'],
      })
      // The second page reuses the snapshot captured by the first one, so both
      // pages describe the same read.
      expect(mocks.list).toHaveBeenNthCalledWith(2, {
        libraryId: 'library-1',
        limit: 25,
        sortBy: 'file_name',
        sortOrder: 'asc',
        includeTotal: false,
        cursor: 'cursor-2',
        search: 'invoice',
        searchStatus: ['failed'],
        normalizedSearch: 'invoice',
        snapshotAt: '2026-11-18T00:00:00Z',
        columnFilters: { status: null },
        status: ['ready'],
      })
      expect(selected(result)).toEqual(['doc-a', 'doc-b', 'doc-c'])
      expect(result.current.expandingSelection).toBe(false)
    } finally {
      unmount()
      queryClient.clear()
    }
  })

  it('applies the header checkbox and Escape to the rows on screen, not to another page', () => {
    const { result, unmount, queryClient } = renderController()
    const visible = [documentItem('doc-a'), documentItem('doc-b')]
    try {
      act(() => {
        result.current.toggleSelection('doc-on-other-page')
      })
      act(() => {
        result.current.toggleAllVisible(true, visible)
      })
      expect(selected(result)).toEqual(['doc-a', 'doc-b', 'doc-on-other-page'])

      act(() => {
        result.current.toggleAllVisible(false, visible)
      })
      expect(selected(result)).toEqual(['doc-on-other-page'])

      act(() => {
        document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))
      })
      expect(selected(result)).toEqual([])
    } finally {
      unmount()
      queryClient.clear()
    }
  })
})
