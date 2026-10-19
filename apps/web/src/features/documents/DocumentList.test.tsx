import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter } from 'react-router-dom'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'
import type { CostSummary } from '@/shared/format'
import type { DocumentItem } from '@/shared/types'

import { DocumentList } from './DocumentList'
import type { useDocumentsPageUrlState } from './components/documents-page/documentsPageState'
import type { useDocumentsListController } from './components/documents-page/useDocumentsListController'
import type { useDocumentsQueries } from './components/documents-page/useDocumentsQueries'
import type { UploadQueueController } from './components/documents-page/useUploadQueueController'

type DocumentsQueries = ReturnType<typeof useDocumentsQueries>
type DocumentsList = ReturnType<typeof useDocumentsListController>
type UrlState = ReturnType<typeof useDocumentsPageUrlState>

type DocumentItemOverrides = { [K in keyof DocumentItem]?: DocumentItem[K] | undefined }

function buildDocument(overrides: DocumentItemOverrides = {}): DocumentItem {
  return {
    id: 'doc-1',
    documentState: 'active',
    fileName: 'handbook.txt',
    fileType: 'text/plain',
    fileSize: 1024,
    uploadedAt: '2026-08-20T09:00:00Z',
    cost: null,
    status: 'ready',
    readiness: 'graph_sparse',
    ...overrides,
  } as DocumentItem
}

function stubPagination(overrides: Record<string, unknown> = {}) {
  return {
    canGoNext: false,
    canGoPrevious: false,
    currentPageNumber: 1,
    goToNextPage: vi.fn(),
    goToPage: vi.fn(),
    goToPreviousPage: vi.fn(),
    pageSize: 50,
    show: false,
    totalPages: null,
    visibleRangeEnd: 0,
    visibleRangeStart: 0,
    ...overrides,
  }
}

function stubDocuments({
  items,
  filteredTotal,
  pagination,
  statusCounts = null,
}: {
  items: DocumentItem[]
  filteredTotal: number | null
  pagination: ReturnType<typeof stubPagination>
  statusCounts?: DocumentsQueries['statusCounts']
}): DocumentsQueries {
  return {
    activateListPollGrace: vi.fn(),
    clearSelectedDoc: vi.fn(),
    debouncedSearch: '',
    errorMessage: (_error: unknown, fallback: string) => fallback,
    filteredTotal,
    isLoading: false,
    isScopePending: false,
    snapshotAt: '2026-10-01T12:00:00Z',
    items,
    libraryCostSummary: null,
    loadError: null,
    loadFirstPage: vi.fn(),
    pagination,
    searchDerivation: { search: '', searchStatus: [], normalizedSearch: null, searchRanges: null },
    selectDoc: vi.fn(),
    selectedDoc: null,
    serverFacets: {
      page: () => undefined,
      onOpen: vi.fn(),
      onSearch: vi.fn(),
      onLoadMore: vi.fn(),
    },
    sortBy: 'uploaded_at',
    sortOrder: 'desc',
    statusCounts,
    totalCount: null,
    webRuns: [],
    webRunsRefreshing: false,
    workspaceCostSummary: null,
  } as unknown as DocumentsQueries
}

function stubList(overrides: Record<string, unknown> = {}) {
  return {
    bulkProgress: null,
    clearSelection: vi.fn(),
    expandingSelection: false,
    handleBulkCancel: vi.fn(),
    handleBulkDelete: vi.fn(),
    handleBulkReprocess: vi.fn(),
    reprocessDocument: vi.fn(),
    selectAllMatching: vi.fn(),
    selectedIds: new Set<string>(),
    setBulkRerun: vi.fn(),
    setSelectedIds: vi.fn(),
    showSelectAllMatching: false,
    toggleAllVisible: vi.fn(),
    toggleDetailColumns: vi.fn(),
    toggleSelection: vi.fn(),
    ...overrides,
  } as unknown as DocumentsList
}

function stubUrl(statusBucket: UrlState['statusBucket'], searchQuery = ''): UrlState {
  return {
    applyGridState: vi.fn(),
    columnFilters: undefined,
    filters: {},
    filtersKey: '',
    gridState: {
      search: searchQuery,
      sort: { columnId: 'uploaded_at', direction: 'desc' },
      filters: {},
      page: 1,
      pageSize: 50,
    },
    listFilter: {},
    pageSize: 50,
    searchQuery,
    selectedDocumentId: null,
    snapshotAt: null,
    sortValue: 'uploaded_at:desc',
    statusBucket,
    updateSearchParamState: vi.fn(),
  } as unknown as UrlState
}

const uploads = {
  pendingUploads: [],
} as unknown as UploadQueueController

function renderList({
  canWrite = false,
  documents,
  list,
  showDetailColumns = false,
  url,
}: {
  canWrite?: boolean
  documents: DocumentsQueries
  list: DocumentsList
  showDetailColumns?: boolean
  url: UrlState
}) {
  return render(
    <MemoryRouter>
      <DocumentList
        canWrite={canWrite}
        documents={documents}
        list={list}
        locale="en"
        showDetailColumns={showDetailColumns}
        uploads={uploads}
        url={url}
      />
    </MemoryRouter>,
  )
}

describe('DocumentList selection actions', () => {
  for (const [locale, label] of [
    ['en', 'Clear selection'],
    ['ru', 'Снять выделение'],
  ] as const) {
    it.each([false, true])(
      `names every bulk action and clears selection in ${locale} with canWrite=%s`,
      async (canWrite) => {
        await i18n.changeLanguage(locale)
        const list = stubList({ selectedIds: new Set(['doc-1']) })
        renderList({
          canWrite,
          documents: stubDocuments({
            items: [buildDocument()],
            filteredTotal: 1,
            pagination: stubPagination(),
          }),
          list,
          url: stubUrl('all'),
        })

        const clear = screen.getByRole('button', { name: label })
        const actions = within(clear.parentElement!).getAllByRole('button')
        expect(actions).toHaveLength(canWrite ? 4 : 1)
        for (const action of actions) expect(action).toHaveAccessibleName()
        clear.focus()
        await userEvent.setup().keyboard('{Enter}')
        expect(list.clearSelection).toHaveBeenCalledTimes(1)
        expect(list.handleBulkCancel).not.toHaveBeenCalled()
        expect(list.handleBulkDelete).not.toHaveBeenCalled()
        expect(list.handleBulkReprocess).not.toHaveBeenCalled()
      },
    )
  }
})

describe('DocumentList empty state', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('en')
  })

  it('keeps unnamed operational rows visible with absent size distinct from zero', () => {
    renderList({
      documents: stubDocuments({
        items: [
          buildDocument({
            fileName: null,
            fileType: null,
            fileSize: null,
            externalKey: 'operational-identity',
          }),
          buildDocument({ id: 'zero-byte', fileName: 'empty.txt', fileSize: 0 }),
        ],
        filteredTotal: 2,
        pagination: stubPagination(),
      }),
      list: stubList(),
      url: stubUrl('all'),
    })
    const absent = screen.getByText('Not available').closest('tr')!
    expect(absent).not.toHaveTextContent('0 B')
    expect(screen.getByText('empty.txt').closest('tr')).toHaveTextContent('0 B')
  })

  it('names a filtered empty set instead of claiming the library is empty', () => {
    renderList({
      documents: stubDocuments({ items: [], filteredTotal: null, pagination: stubPagination() }),
      list: stubList(),
      url: stubUrl('failed'),
    })

    // "No documents yet" is the true-empty-library sentence; a status filter
    // that matched nothing is a different statement.
    expect(screen.queryByText('No documents yet')).not.toBeInTheDocument()
    expect(screen.getByText('No documents in this filter')).toBeInTheDocument()
  })

  it('names an empty indexing-gap set as no gaps, not a missing library', () => {
    renderList({
      documents: stubDocuments({ items: [], filteredTotal: null, pagination: stubPagination() }),
      list: stubList(),
      url: stubUrl('vectorgap'),
    })

    expect(screen.queryByText('No documents yet')).not.toBeInTheDocument()
    expect(screen.getByText('No documents are missing their semantic index')).toBeInTheDocument()
  })

  it('keeps the true-empty library message for an unfiltered list', () => {
    renderList({
      documents: stubDocuments({ items: [], filteredTotal: 0, pagination: stubPagination() }),
      list: stubList(),
      url: stubUrl('all'),
    })

    expect(screen.getByText('No documents yet')).toBeInTheDocument()
  })
})

describe('DocumentList pagination summary', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('en')
  })

  it('does not present the old scope count under a pending new search', () => {
    renderList({
      documents: {
        ...stubDocuments({
          items: [buildDocument()],
          filteredTotal: 21,
          pagination: stubPagination(),
          statusCounts: {
            total: 21,
            ready: 21,
            queued: 0,
            processing: 0,
            canceled: 0,
            failed: 0,
            empty: 0,
          },
        }),
        isScopePending: true,
      },
      list: stubList(),
      url: stubUrl('all', 'new search'),
    })
    expect(screen.queryByText('21 rows')).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Ready' })).toBeInTheDocument()
    expect(screen.getByRole('table')).toHaveAttribute('aria-busy', 'true')
  })

  it('omits the total when the filtered total is unknown', () => {
    // The vector/graph gap buckets carry no count in the status aggregate.
    // Printing the visible range end as a total states a number that is not
    // this set's size.
    const items = [buildDocument({ id: 'a' }), buildDocument({ id: 'b' })]
    renderList({
      documents: stubDocuments({
        items,
        filteredTotal: null,
        pagination: stubPagination({ show: true, visibleRangeEnd: 2, visibleRangeStart: 1 }),
      }),
      list: stubList(),
      url: stubUrl('graphgap'),
    })

    expect(screen.getByText('1–2')).toBeInTheDocument()
    expect(screen.queryByText('1–2 of 2')).not.toBeInTheDocument()
  })

  it('keeps the total when the filtered total is known', () => {
    const items = [buildDocument({ id: 'a' }), buildDocument({ id: 'b' })]
    renderList({
      documents: stubDocuments({
        items,
        filteredTotal: 42,
        pagination: stubPagination({ show: true, visibleRangeEnd: 2, visibleRangeStart: 1 }),
      }),
      list: stubList(),
      url: stubUrl('all'),
    })

    expect(screen.getByText('1–2 of 42')).toBeInTheDocument()
  })
})

describe('DocumentList column headers', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('en')
  })

  it('sends a detail-column sort as a server sort, not a page-scoped reorder', async () => {
    // Regression: Cost used to be a page-local sort that never reached the
    // list request. The header must now drive the shared grid state, whose sort
    // is what crosses the wire.
    const applyGridState = vi.fn()
    const url = { ...stubUrl('all'), applyGridState } as unknown as UrlState
    renderList({
      documents: stubDocuments({
        items: [buildDocument({ id: 'a' })],
        filteredTotal: 10,
        pagination: stubPagination({ show: true, visibleRangeEnd: 1, visibleRangeStart: 1 }),
      }),
      list: stubList(),
      showDetailColumns: true,
      url,
    })

    await userEvent
      .setup()
      .click(within(screen.getByRole('columnheader', { name: /Cost/ })).getByRole('button'))
    await userEvent.setup().click(screen.getByRole('menuitem', { name: /ascending/i }))

    expect(applyGridState).toHaveBeenCalledOnce()
    expect(applyGridState.mock.calls[0]![0](url.gridState)).toEqual(
      expect.objectContaining({ sort: { columnId: 'cost', direction: 'asc' } }),
    )
  })
})

describe('DocumentList cost freshness', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('en')
  })

  const cases: {
    name: string
    summary: CostSummary | null
    exactTitle?: string
    display?: string
  }[] = [
    { name: 'absent summary', summary: null },
    { name: 'missing currency', summary: { costRollupFreshness: 'current', totalCost: '1.23' } },
    {
      name: 'known zero',
      summary: {
        costRollupFreshness: 'current',
        totalCost: '0',
        currencyCode: 'USD',
        costCoverage: 'complete',
      },
      exactTitle: '0 USD',
      display: 'USD 0.00',
    },
    {
      name: 'partial exact decimal',
      summary: {
        costRollupFreshness: 'current',
        totalCost: '9007199254740993.000000000000000001',
        currencyCode: 'USD',
        costCoverage: 'partial',
        unpricedProviderCallCount: 2,
      },
      exactTitle: '9007199254740993.000000000000000001 USD',
      display: '≥ USD 9,007,199,254,740,993.00',
    },
    {
      name: 'stale amount in a reconciling summary',
      summary: {
        costRollupFreshness: 'reconciling',
        totalCost: '123.45',
        currencyCode: 'USD',
        unpricedProviderCallCount: 2,
      },
      display: 'Updating…',
    },
  ]

  it.each(cases)('preserves cost text and title for $name', ({ summary, exactTitle, display }) => {
    const { container } = renderList({
      documents: {
        ...stubDocuments({
          items: [buildDocument()],
          filteredTotal: 1,
          pagination: stubPagination(),
        }),
        libraryCostSummary: summary,
      },
      list: stubList(),
      url: stubUrl('all'),
    })
    const unpricedSuffix =
      exactTitle && summary?.unpricedProviderCallCount
        ? ` · ${summary.unpricedProviderCallCount} ${i18n.t('documents.unpriced')}`
        : ''
    const expectedTitle = exactTitle
      ? `${exactTitle}${unpricedSuffix}`
      : (display ?? i18n.t('common.costUnknown'))
    const cell = within(container).getByTitle(`Library cost: ${expectedTitle}`)
    expect(within(cell).getByText(display ?? i18n.t('common.costUnknown'))).toBeInTheDocument()
    if (summary?.costRollupFreshness === 'reconciling') {
      expect(cell.textContent).not.toContain('123.45')
      expect(cell.title).not.toContain('unpriced')
    }
  })

  it('marks a reconciling cost cell as updating, never a fabricated amount', () => {
    const { container } = renderList({
      documents: {
        ...stubDocuments({
          items: [buildDocument()],
          filteredTotal: 1,
          pagination: stubPagination(),
        }),
        libraryCostSummary: {
          costRollupFreshness: 'reconciling',
          totalCost: null,
          currencyCode: null,
          providerCallCount: null,
          unpricedProviderCallCount: null,
          costCoverage: null,
        },
      } as unknown as DocumentsQueries,
      list: stubList(),
      url: stubUrl('all'),
    })

    expect(within(container).getByTitle(/Library cost: Updating/)).toBeInTheDocument()
    expect(container.textContent).not.toContain('\u2265')
  })
})
