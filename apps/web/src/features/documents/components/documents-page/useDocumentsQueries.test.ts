import { act, renderHook, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { createElement, type ReactNode } from 'react'
import { MemoryRouter } from 'react-router-dom'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { queries, type DocumentListPageResponse } from '@/shared/api'
import i18n from '@/shared/i18n'
import { useDocumentsPageUrlState } from './documentsPageState'
import { buildDocumentListQuery, useDocumentsQueries } from './useDocumentsQueries'

afterEach(() => vi.restoreAllMocks())

describe('buildDocumentListQuery', () => {
  it('carries a cost sort across a cursor page as sortBy=cost, not a page-local reorder', () => {
    const query = buildDocumentListQuery({
      cursor: 'cursor-2',
      pageSize: 50,
      sortValue: 'cost:desc',
      derivation: { search: '', searchStatus: [], normalizedSearch: null, searchRanges: null },
      columnFilters: { cost: ['0.10'], status: ['ready'] },
      listFilter: {},
      snapshotAt: 'T0',
    })
    expect(query).toMatchObject({
      cursor: 'cursor-2',
      limit: 50,
      sortBy: 'cost',
      sortOrder: 'desc',
      filters: '{"cost":["0.10"],"status":["ready"]}',
      snapshotAt: 'T0',
    })
  })

  it('forwards the derived search buckets, the normalized term and the status pill', () => {
    const query = buildDocumentListQuery({
      cursor: null,
      pageSize: 25,
      sortValue: 'uploaded_at:desc',
      derivation: {
        search: '0,50',
        searchStatus: ['ready'],
        normalizedSearch: '0.50',
        searchRanges: { pipelineTime: { min: 90_000, max: 91_000 } },
      },
      columnFilters: undefined,
      listFilter: { status: ['processing', 'queued'], indexing: 'vector_gap' },
      snapshotAt: null,
    })
    expect(query).toMatchObject({
      search: '0,50',
      searchStatus: 'ready',
      normalizedSearch: '0.50',
      searchRanges: '{"pipelineTime":{"min":90000,"max":91000}}',
      status: 'processing,queued',
      indexing: 'vector_gap',
    })
    expect(query.cursor).toBeUndefined()
    expect(query.filters).toBeUndefined()
    expect(query.snapshotAt).toBeUndefined()
  })

  it('omits the status pill for the indexing buckets that carry no status', () => {
    const query = buildDocumentListQuery({
      cursor: null,
      pageSize: 50,
      sortValue: 'finished_at:asc',
      derivation: { search: '', searchStatus: [], normalizedSearch: null, searchRanges: null },
      columnFilters: undefined,
      listFilter: { indexing: 'graph_gap' },
      snapshotAt: null,
    })
    expect(query.status).toBeUndefined()
    expect(query.indexing).toBe('graph_gap')
    expect(query.sortBy).toBe('finished_at')
    expect(query.sortOrder).toBe('asc')
  })
})

describe('server snapshot adoption', () => {
  it('ignores a late list response from the previous library after the new scope pins its clock', async () => {
    const oldSnapshot = '2026-10-01T12:00:00Z'
    const newSnapshot = '2026-10-01T13:00:00Z'
    let finishOld!: (page: DocumentListPageResponse) => void
    const oldPage = new Promise<DocumentListPageResponse>((resolve) => {
      finishOld = resolve
    })
    const listOptions = queries.listContentDocumentsOptions
    const optionsSpy = vi
      .spyOn(queries, 'listContentDocumentsOptions')
      .mockImplementation((options) => ({
        ...listOptions(options),
        queryFn: async () => {
          if (options.path.libraryId === 'old' && options.query?.limit === 50) return oldPage
          return {
            items: [],
            snapshotAt:
              options.query?.snapshotAt ??
              (options.path.libraryId === 'old' ? oldSnapshot : newSnapshot),
            totalCount: 0,
            costRollupFreshness: 'current',
            scopePending: false,
          }
        },
      }))
    vi.spyOn(globalThis, 'fetch').mockImplementation(async () => Response.json({ items: [] }))
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
    function Wrapper({ children }: Readonly<{ children: ReactNode }>) {
      return createElement(
        QueryClientProvider,
        { client },
        createElement(
          MemoryRouter,
          { initialEntries: [`/documents?snapshotAt=${oldSnapshot}&documentId=old-selection`] },
          children,
        ),
      )
    }
    const { result, rerender, unmount } = renderHook(
      ({ libraryId }) => {
        const url = useDocumentsPageUrlState({
          libraryId,
          tableState: {
            pageSize: 50,
            sort: { key: 'uploaded_at', direction: 'desc' },
            showDetailColumns: false,
          },
          setTableState: vi.fn(),
        })
        const documents = useDocumentsQueries({
          activeLibrary: {
            id: libraryId,
            workspaceId: 'workspace',
            name: libraryId,
            createdAt: oldSnapshot,
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
        return { url, documents }
      },
      { initialProps: { libraryId: 'old' }, wrapper: Wrapper },
    )
    await waitFor(() => expect(optionsSpy).toHaveBeenCalled())
    expect(result.current.url.snapshotAt).toBe(oldSnapshot)
    rerender({ libraryId: 'new' })
    await waitFor(() => expect(result.current.url.snapshotAt).toBe(newSnapshot))
    expect(result.current.url.selectedDocumentId).toBeNull()
    const newScopeQueries = optionsSpy.mock.calls
      .map(([options]) => options)
      .filter((options) => options.path.libraryId === 'new')
    expect(newScopeQueries.length).toBeGreaterThan(0)
    expect(newScopeQueries.some((options) => options.query?.snapshotAt === oldSnapshot)).toBe(false)
    expect(newScopeQueries.some((options) => options.query?.ids === 'old-selection')).toBe(false)
    await act(async () =>
      finishOld({
        items: [],
        snapshotAt: oldSnapshot,
        totalCount: 0,
        costRollupFreshness: 'current',
        scopePending: false,
      }),
    )
    expect(result.current.url.snapshotAt).toBe(newSnapshot)
    expect(result.current.documents.snapshotAt).toBe(newSnapshot)
    unmount()
    client.clear()
  })
})
