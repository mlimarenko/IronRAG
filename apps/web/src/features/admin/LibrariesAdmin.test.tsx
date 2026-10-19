import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { createMemoryRouter, RouterProvider } from 'react-router-dom'
import { toast } from 'sonner'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'
import type { CatalogLibraryResponse } from '@/shared/api/generated'
import type { AdminLibrariesFilters } from './libraries/api/useLibrariesAdmin'
import type { AdminLibrariesPage, AdminLibraryRow } from './libraries/model/librariesAdapter'
import LibrariesAdmin from './LibrariesAdmin'

const workspace = { displayName: 'Workspace One', id: 'workspace-1', slug: 'workspace-one' }

const readyRow: AdminLibraryRow = {
  costSummary: {
    costCoverage: 'complete',
    currencyCode: 'USD',
    documentCount: 12,
    providerCallCount: 40,
    totalCost: '1.5',
    unpricedProviderCallCount: 0,
  },
  displayName: 'Library One',
  queuedJobCount: 7,
  id: 'library-1',
  includeDocumentHintInMcpAnswers: true,
  lifecycleState: 'active',
  readiness: { missingBindingPurposes: [], ready: true },
  slug: 'library-one',
  workspace,
}

const unknownCostRow: AdminLibraryRow = {
  costSummary: null,
  displayName: 'Library Two',
  queuedJobCount: 0,
  id: 'library-2',
  includeDocumentHintInMcpAnswers: true,
  lifecycleState: 'active',
  readiness: { missingBindingPurposes: [], ready: true },
  slug: 'library-two',
  workspace,
}

const blockedRow: AdminLibraryRow = {
  costSummary: null,
  displayName: 'Library Three',
  queuedJobCount: 0,
  id: 'library-3',
  includeDocumentHintInMcpAnswers: true,
  lifecycleState: 'archived',
  readiness: {
    missingBindingPurposes: ['extract_text', 'extract_graph', 'embed_chunk'],
    ready: false,
  },
  slug: 'library-three',
  workspace,
}

const catalogLibrary: CatalogLibraryResponse = {
  description: null,
  displayName: readyRow.displayName,
  extractionPrompt: null,
  id: readyRow.id,
  includeDocumentHintInMcpAnswers: true,
  ingestionReadiness: { missingBindingPurposes: [], ready: true },
  lifecycleState: 'active',
  recognitionPolicy: { rasterImageEngine: 'vision' },
  retrievalConfig: { lexical: { textSearchConfig: 'english' } },
  runtimeReadiness: { missingBindingPurposes: [] },
  slug: readyRow.slug,
  webIngestPolicy: {
    crawlFilter: { allowPatterns: [], blockPatterns: [] },
    materializationFilter: { allowPatterns: [], blockPatterns: [] },
  },
  workspaceId: workspace.id,
}

const page = (items: AdminLibraryRow[], totalCount = items.length): AdminLibrariesPage => ({
  aggregateCostSummary: readyRow.costSummary,
  facets: { activeCount: items.length, readyCount: 1, workspaceCount: 1 },
  items,
  totalCount,
})

const mocks = vi.hoisted(() => {
  const state: {
    latestFilters: AdminLibrariesFilters | null
    listState: 'loading' | 'error' | 'ready'
    page: AdminLibrariesPage | null
  } = {
    latestFilters: null,
    listState: 'ready',
    page: null,
  }
  return {
    ...state,
    deleteLibrary: vi.fn(),
    downloadLibrarySnapshot: vi.fn(),
    downloadWorkspaceSnapshot: vi.fn(),
    loadLibrary: vi.fn(),
    refetch: vi.fn(),
    refreshSession: vi.fn(),
    restoreSnapshot: vi.fn(),
    saveSettings: vi.fn(),
    setActiveLibrary: vi.fn(),
  }
})

vi.mock('sonner', () => ({
  toast: {
    error: vi.fn(),
    success: vi.fn(),
    warning: vi.fn(),
  },
}))

vi.mock('@/shared/contexts/app-context', () => ({
  useApp: () => ({
    locale: 'en',
    refreshSession: mocks.refreshSession,
    setActiveLibrary: mocks.setActiveLibrary,
  }),
}))

vi.mock('@/features/admin/libraries/api/useLibrariesAdmin', () => ({
  downloadLibrarySnapshot: (...args: unknown[]) => mocks.downloadLibrarySnapshot(...args),
  downloadWorkspaceSnapshot: (...args: unknown[]) => mocks.downloadWorkspaceSnapshot(...args),
  useDebouncedValue: <T,>(value: T) => value,
  useLibraryFacets: () => ({
    serverFacets: {
      page: () => undefined,
      onOpen: vi.fn(),
      onSearch: vi.fn(),
      onLoadMore: vi.fn(),
    },
  }),
  useAdminLibrariesQuery: (filters: AdminLibrariesFilters) => {
    mocks.latestFilters = filters
    if (mocks.listState === 'loading') {
      return {
        data: undefined,
        isError: false,
        isFetching: false,
        isLoading: true,
        refetch: mocks.refetch,
      }
    }
    if (mocks.listState === 'error') {
      return {
        data: undefined,
        isError: true,
        isFetching: false,
        isLoading: false,
        refetch: mocks.refetch,
      }
    }
    const data = mocks.page
    if (!data) {
      return {
        data: undefined,
        isError: false,
        isFetching: false,
        isLoading: false,
        refetch: mocks.refetch,
      }
    }
    if (filters.search === 'nomatch') {
      return {
        data: { ...data, items: [], totalCount: 0 },
        isError: false,
        isFetching: false,
        isLoading: false,
        refetch: mocks.refetch,
      }
    }
    if (filters.offset >= 50) {
      return {
        data: { ...data, items: [unknownCostRow], totalCount: data.totalCount },
        isError: false,
        isFetching: false,
        isLoading: false,
        refetch: mocks.refetch,
      }
    }
    return { data, isError: false, isFetching: false, isLoading: false, refetch: mocks.refetch }
  },
  useLibraryAdminMutations: () => ({
    deleteLibrary: { isPending: false, mutateAsync: mocks.deleteLibrary },
    loadLibrary: mocks.loadLibrary,
    restoreSnapshot: { isPending: false, mutateAsync: mocks.restoreSnapshot },
    saveSettings: { isPending: false, mutateAsync: mocks.saveSettings },
  }),
  useLibraryInspectorQueries: () => ({
    completedJobs: { data: { items: [] }, isError: false, isLoading: false },
    failedJobs: { data: { items: [] }, isError: false, isLoading: false },
    library: { data: catalogLibrary, isError: false, isLoading: false },
    queue: { data: { summary: { total: 0 } }, isError: false, isLoading: false },
    retrieval: {
      data: { lexical: { textSearchConfig: 'english' } },
      isError: false,
      isLoading: false,
    },
    state: {
      data: { state: { degradedState: 'healthy', failedDocumentCount: 0 } },
      isError: false,
      isLoading: false,
    },
  }),
}))

function renderLibraries() {
  const router = createMemoryRouter(
    [
      { element: <LibrariesAdmin />, path: '/admin/libraries' },
      { element: <div>Documents page</div>, path: '/documents' },
      { element: <div>AI admin</div>, path: '/admin/ai' },
      { element: <div>Library queue</div>, path: '/admin/queue' },
    ],
    { initialEntries: ['/admin/libraries'] },
  )
  return { router, ...render(<RouterProvider router={router} />) }
}

function libraryRow(name: RegExp): HTMLTableRowElement {
  const table = screen.getByRole('table')
  const row = within(table).getByRole('button', { name }).closest('tr')
  if (!row) throw new Error('library row did not render')
  return row
}

describe('LibrariesAdmin', () => {
  beforeEach(async () => {
    vi.clearAllMocks()
    mocks.listState = 'ready'
    mocks.page = page([readyRow, unknownCostRow, blockedRow], 3)
    mocks.loadLibrary.mockResolvedValue(catalogLibrary)
    mocks.saveSettings.mockResolvedValue(undefined)
    mocks.deleteLibrary.mockResolvedValue(undefined)
    mocks.restoreSnapshot.mockResolvedValue(undefined)
    mocks.refreshSession.mockResolvedValue(undefined)
    await i18n.changeLanguage('en')
  })

  it('links a nonzero queued count to the exact library without changing the shell selection', async () => {
    const { router } = renderLibraries()
    const link = within(screen.getByRole('table')).getByRole('link', {
      name: 'Open queue for Library One — jobs: 7',
    })
    expect(link).toHaveTextContent('7')
    await userEvent.setup().click(link)
    expect(router.state.location.pathname).toBe('/admin/queue')
    expect(new URLSearchParams(router.state.location.search).get('libraryId')).toBe(readyRow.id)
    expect(new URLSearchParams(router.state.location.search).get('status')).toBe('queued')
    expect(mocks.setActiveLibrary).not.toHaveBeenCalled()
  })

  it('renders a zero queued count as non-interactive text', () => {
    renderLibraries()
    const row = libraryRow(/Library Two/)
    expect(within(row).queryByRole('link', { name: /Open queue/ })).not.toBeInTheDocument()
    const queueColumn = within(screen.getByRole('table'))
      .getAllByRole('columnheader')
      .findIndex((header) => header.dataset.column === 'queue')
    expect(queueColumn).toBeGreaterThanOrEqual(0)
    const queueCell = within(row).getAllByRole('cell')[queueColumn]
    expect(queueCell).toHaveTextContent('0')
    expect(queueCell?.querySelector('a, button, [tabindex]')).toBeNull()
  })

  it('shows the loading state', () => {
    mocks.listState = 'loading'
    mocks.page = null
    renderLibraries()

    expect(screen.getByRole('table')).toHaveAttribute('aria-busy', 'true')
  })

  it('shows a load error', () => {
    mocks.listState = 'error'
    mocks.page = null
    renderLibraries()

    expect(screen.getByText('Failed to load libraries')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Try again' })).toBeInTheDocument()
  })

  it('shows the empty directory', () => {
    mocks.page = page([], 0)
    renderLibraries()

    expect(screen.getByText('No libraries found.')).toBeInTheDocument()
  })

  it('shows an empty filtered state after search', async () => {
    const user = userEvent.setup()
    renderLibraries()
    await user.type(screen.getByPlaceholderText('Search libraries or workspaces...'), 'nomatch')

    await waitFor(() => {
      expect(screen.getByText('No libraries match the search.')).toBeInTheDocument()
    })
  })

  it('renders metrics and the library table', () => {
    renderLibraries()

    expect(screen.getByText('Workspaces')).toBeInTheDocument()
    expect(screen.getByText('1 ready · 2 blocked')).toBeInTheDocument()
    expect(screen.getAllByText('USD 1.50').length).toBeGreaterThan(0)
    expect(screen.getByText('40 provider calls')).toBeInTheDocument()
    expect(screen.getAllByRole('button', { name: /Library One/ }).length).toBeGreaterThan(0)
  })

  it('renders unknown cost placeholders', () => {
    renderLibraries()

    expect(screen.getAllByText('Cost unknown').length).toBeGreaterThan(0)
    expect(screen.getAllByText('Not available').length).toBeGreaterThan(0)
  })

  it('summarizes extra blocked binding purposes with +1', () => {
    renderLibraries()

    expect(screen.getByText(/Document Understanding, Graph Extraction \+1/)).toBeInTheDocument()
  })

  it('debounces search into the libraries query', async () => {
    const user = userEvent.setup()
    renderLibraries()
    await user.type(screen.getByPlaceholderText('Search libraries or workspaces...'), 'alpha')

    await waitFor(() => {
      expect(mocks.latestFilters).toMatchObject({ search: 'alpha' })
    })
  })

  it('pages to the next offset', async () => {
    const user = userEvent.setup()
    mocks.page = page([readyRow], 51)
    renderLibraries()

    expect(screen.getByText('1–1 of 51')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Next' }))
    expect(mocks.latestFilters).toMatchObject({ offset: 50 })
    expect(screen.getAllByText('Library Two').length).toBeGreaterThan(0)
  })

  it('opens the inspector from the library name', async () => {
    const user = userEvent.setup()
    renderLibraries()
    await user.click(within(libraryRow(/Library One/)).getByRole('button', { name: /Library One/ }))

    expect(screen.getByRole('dialog', { name: 'Library One' })).toBeInTheDocument()
  })

  it('opens documents after loading the library', async () => {
    const user = userEvent.setup()
    const { router } = renderLibraries()
    await user.click(within(libraryRow(/Library One/)).getByRole('button', { name: 'Open' }))

    await waitFor(() => {
      expect(mocks.loadLibrary).toHaveBeenCalledWith(readyRow.id)
    })
    expect(mocks.setActiveLibrary).toHaveBeenCalled()
    expect(router.state.location.pathname).toBe('/documents')
  })

  it('explains a failed documents navigation', async () => {
    const user = userEvent.setup()
    mocks.loadLibrary.mockRejectedValueOnce(new Error('library missing'))
    renderLibraries()
    await user.click(within(libraryRow(/Library One/)).getByRole('button', { name: 'Open' }))

    await waitFor(() => {
      expect(toast.error).toHaveBeenCalledWith('library missing')
    })
  })

  it('navigates to AI configuration for the library', async () => {
    const user = userEvent.setup()
    const { router } = renderLibraries()
    await user.click(within(libraryRow(/Library One/)).getByTitle('Configure AI'))

    expect(router.state.location.pathname).toBe('/admin/ai')
    expect(router.state.location.search).toBe(`?scope=library&lib=${readyRow.id}`)
  })

  it('honors a cancelled row delete confirmation', async () => {
    const user = userEvent.setup()
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false)
    renderLibraries()
    await user.click(within(libraryRow(/Library One/)).getByTitle('Delete'))

    expect(confirm).toHaveBeenCalled()
    expect(mocks.deleteLibrary).not.toHaveBeenCalled()
    confirm.mockRestore()
  })

  it('deletes a library after confirmation', async () => {
    const user = userEvent.setup()
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true)
    renderLibraries()
    await user.click(within(libraryRow(/Library One/)).getByTitle('Delete'))

    await waitFor(() => {
      expect(mocks.deleteLibrary).toHaveBeenCalledWith({
        libraryId: readyRow.id,
        workspaceId: workspace.id,
      })
    })
    expect(toast.success).toHaveBeenCalledWith('Deleted 1 library.')
    confirm.mockRestore()
  })

  it('explains a failed library delete', async () => {
    const user = userEvent.setup()
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true)
    mocks.deleteLibrary.mockRejectedValueOnce(new Error('delete failed'))
    renderLibraries()
    await user.click(within(libraryRow(/Library One/)).getByTitle('Delete'))

    await waitFor(() => {
      expect(toast.error).toHaveBeenCalledWith('delete failed')
    })
    confirm.mockRestore()
  })

  it('saves inspector settings', async () => {
    const user = userEvent.setup()
    renderLibraries()
    await user.click(within(libraryRow(/Library One/)).getByRole('button', { name: /Library One/ }))
    const dialog = await screen.findByRole('dialog', { name: 'Library One' })
    await user.click(within(dialog).getByRole('button', { name: 'Save' }))

    await waitFor(() => {
      expect(mocks.saveSettings).toHaveBeenCalled()
    })
    expect(toast.success).toHaveBeenCalledWith('Library settings saved.')
    expect(mocks.refreshSession).toHaveBeenCalled()
  })

  it('exports a library snapshot', async () => {
    const user = userEvent.setup()
    renderLibraries()
    await user.click(within(libraryRow(/Library One/)).getByTitle('Backup'))
    const dialog = await screen.findByRole('dialog', { name: 'Library backup' })
    await user.click(within(dialog).getByRole('button', { name: 'Export library' }))

    expect(mocks.downloadLibrarySnapshot).toHaveBeenCalledWith(readyRow.id, [
      'library_data',
      'blobs',
    ])
    expect(toast.success).toHaveBeenCalledWith('Library backup download started.')
  })

  it('restores a library snapshot', async () => {
    const user = userEvent.setup()
    renderLibraries()
    await user.click(within(libraryRow(/Library One/)).getByTitle('Restore'))
    const dialog = await screen.findByRole('dialog', { name: 'Library restore' })
    const file = new File(['archive'], 'library.tar.zst', { type: 'application/zstd' })
    const input = dialog.querySelector('input[type="file"]')
    if (!input) throw new Error('snapshot file input did not render')
    await user.upload(input as HTMLInputElement, file)
    await user.click(within(dialog).getByRole('button', { name: 'Restore' }))

    await waitFor(() => {
      expect(mocks.restoreSnapshot).toHaveBeenCalledWith(
        expect.objectContaining({
          file,
          libraryId: readyRow.id,
          overwrite: 'reject',
          scope: 'library',
          workspaceId: workspace.id,
        }),
      )
    })
    expect(toast.success).toHaveBeenCalledWith('Library restore completed.')
  })

  it('closes the inspector', async () => {
    const user = userEvent.setup()
    renderLibraries()
    await user.click(within(libraryRow(/Library One/)).getByRole('button', { name: /Library One/ }))
    const dialog = await screen.findByRole('dialog', { name: 'Library One' })
    await user.click(within(dialog).getAllByRole('button', { name: 'Close' })[0]!)

    await waitFor(() => {
      expect(screen.queryByRole('dialog', { name: 'Library One' })).not.toBeInTheDocument()
    })
  })
})
