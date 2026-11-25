import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import type { DocumentItem, Library, Workspace } from '@/shared/types'

import DocumentsPage from './DocumentsPage'

// The page owns composition, permissions, the page-level queries and the URL
// contract. Everything that fetches is faked at its existing hook boundary;
// the child panels (DocumentList, UploadDialog, WebIngestPanel) and the upload
// queue controller are the real ones, each already covered by its own test.
const mocks = vi.hoisted(() => {
  const app: {
    activeLibrary: Library | null
    activeWorkspace: Workspace | null
    libraries: Library[]
    locale: 'en'
    user: { role: 'admin' | 'operator' | 'viewer' }
  } = {
    activeLibrary: null,
    activeWorkspace: null,
    libraries: [],
    locale: 'en',
    user: { role: 'admin' },
  }
  return {
    app,
    documents: {} as ReturnType<typeof stubDocuments>,
    list: {} as ReturnType<typeof stubList>,
    web: {} as ReturnType<typeof stubWebIngest>,
  }
})

vi.mock('@/shared/contexts/app-context', () => ({
  useApp: () => mocks.app,
}))

// ChooseLibraryButton admits exactly the role `AdminLayout` admits, and the
// preferences provider lives in the shell this page renders without.
vi.mock('@/app/preferences-context', () => ({
  usePreferences: () => ({ role: 'admin' }),
}))

vi.mock('./components/documents-page/useDocumentsQueries', () => ({
  useDocumentsQueries: () => mocks.documents,
}))

vi.mock('./components/documents-page/useDocumentsListController', () => ({
  useDocumentsListController: () => mocks.list,
}))

vi.mock('./components/documents-page/useWebIngestController', () => ({
  useWebIngestController: () => mocks.web,
}))

// The panel and its dialog are covered by WebIngestPanel.test.tsx; here the page
// only has to decide which panel is mounted and with which permission.
vi.mock('./WebIngestPanel', () => ({
  WebIngestPanel: ({ canWrite }: { canWrite: boolean }) => (
    <div>{`web panel canWrite=${String(canWrite)}`}</div>
  ),
}))

function buildLibrary(overrides: Partial<Library> = {}): Library {
  return {
    id: 'library-1',
    workspaceId: 'workspace-1',
    name: 'Example library',
    createdAt: '2026-01-01T00:00:00Z',
    includeDocumentHintInMcpAnswers: false,
    ingestionReady: true,
    queryReady: true,
    missingBindingPurposes: [],
    ...overrides,
  }
}

function buildDocument(overrides: Partial<DocumentItem> = {}): DocumentItem {
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
  }
}

function stubPagination() {
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
  }
}

function stubDocuments(overrides: Record<string, unknown> = {}) {
  return {
    activateListPollGrace: vi.fn(),
    clearSelectedDoc: vi.fn(),
    debouncedSearch: '',
    errorMessage: (_error: unknown, fallback: string) => fallback,
    fetchLibraryWebIngestPolicy: vi.fn(async () => null),
    fetchSelectedDetail: vi.fn(async () => {}),
    filteredTotal: null as number | null,
    // An all-zero ledger, so the cost strip starts hidden.
    libraryCost: 0 as number | null,
    libraryCostCoverage: null,
    libraryCostFreshness: null,
    libraryCostSummary: null,
    libraryPolicyQuery: { data: undefined, isLoading: false },
    libraryUnpricedCalls: 0 as number | null,
    loadError: null as string | null,
    loadFirstPage: vi.fn(async () => {}),
    isLoading: false,
    isScopePending: false,
    items: [] as DocumentItem[],
    pagination: stubPagination(),
    refreshWebRuns: vi.fn(async () => {}),
    searchDerivation: {
      search: '',
      searchStatus: [] as string[],
      normalizedSearch: null,
      searchRanges: null,
    },
    selectDoc: vi.fn(),
    selectedDoc: null as DocumentItem | null,
    serverFacets: {
      page: () => undefined,
      onOpen: vi.fn(),
      onSearch: vi.fn(),
      onLoadMore: vi.fn(),
    },
    snapshotAt: null as string | null,
    sortBy: 'uploaded_at',
    sortOrder: 'desc',
    statusCounts: null,
    totalCount: null as number | null,
    updateDocumentHintLocally: vi.fn(),
    webRuns: [] as Array<Record<string, unknown>>,
    webRunsRefreshing: false,
    workspaceCost: 0 as number | null,
    workspaceCostCoverage: null,
    workspaceCostFreshness: null,
    workspaceCostSummary: null,
    workspaceUnpricedCalls: 0 as number | null,
    ...overrides,
  }
}

function stubList() {
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
  }
}

function stubWebIngest() {
  return {
    openCreateDialog: vi.fn(),
    refreshWebRuns: vi.fn(async () => {}),
    webRuns: [] as Array<Record<string, unknown>>,
    webRunsRefreshing: false,
  }
}

function renderPage() {
  return render(
    <MemoryRouter initialEntries={['/documents']}>
      <Routes>
        <Route path="/documents" element={<DocumentsPage />} />
        <Route path="/admin/ai" element={<h1>AI settings</h1>} />
      </Routes>
    </MemoryRouter>,
  )
}

beforeEach(async () => {
  mocks.app.activeLibrary = null
  mocks.app.libraries = []
  mocks.app.user.role = 'admin'
  mocks.documents = stubDocuments()
  mocks.list = stubList()
  mocks.web = stubWebIngest()
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe('DocumentsPage without a library', () => {
  it('says nothing is selected and offers the create action instead of a document list', () => {
    renderPage()

    expect(screen.getByRole('heading', { level: 1, name: 'Documents' })).toBeInTheDocument()
    expect(screen.getByText('No library selected')).toBeInTheDocument()
    expect(screen.getByText('Select or create a library to manage documents.')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Create a library' })).toBeInTheDocument()
    expect(screen.queryByRole('tablist')).not.toBeInTheDocument()
  })
})

describe('DocumentsPage for an empty library', () => {
  beforeEach(() => {
    mocks.app.activeLibrary = buildLibrary()
    mocks.app.libraries = [buildLibrary()]
    mocks.documents = stubDocuments({ totalCount: 0, filteredTotal: 0 })
  })

  it('names the page, both tabs and the upload route out of the empty state', async () => {
    const filePicker = vi.spyOn(HTMLInputElement.prototype, 'click')
    renderPage()

    expect(screen.getByRole('heading', { level: 1, name: 'Documents' })).toBeInTheDocument()
    expect(screen.getByRole('tab', { name: 'Documents' })).toHaveAttribute('aria-selected', 'true')
    expect(screen.getByRole('tab', { name: 'Web Ingest' })).toBeInTheDocument()
    expect(screen.getByText('No documents yet')).toBeInTheDocument()

    await userEvent.setup().click(screen.getByRole('button', { name: 'Upload documents' }))
    expect(filePicker).toHaveBeenCalledTimes(1)

    await userEvent.setup().click(screen.getByRole('button', { name: 'Add content' }))
    expect(filePicker).toHaveBeenCalledTimes(2)

    await userEvent.setup().click(screen.getByRole('button', { name: 'Refresh' }))
    expect(mocks.documents.loadFirstPage).toHaveBeenCalledTimes(1)

    filePicker.mockRestore()
  })

  it('offers no upload affordance to a viewer who cannot upload', () => {
    mocks.app.user.role = 'viewer'
    renderPage()

    expect(screen.getByText('No documents yet')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Upload documents' })).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Add content' })).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Refresh' })).toBeInTheDocument()
  })
})

describe('DocumentsPage for a populated library', () => {
  beforeEach(() => {
    mocks.app.activeLibrary = buildLibrary()
    mocks.app.libraries = [buildLibrary()]
    mocks.documents = stubDocuments({
      filteredTotal: 1,
      items: [buildDocument()],
      totalCount: 1,
    })
  })

  it('renders the document list instead of the empty state', () => {
    renderPage()

    expect(screen.getByText('handbook.txt')).toBeInTheDocument()
    expect(screen.queryByText('No documents yet')).not.toBeInTheDocument()
  })

  it('carries the page-level cost decision into the list wrapper', () => {
    const { container } = renderPage()
    expect(container.querySelector('.tab-panel > div')?.className).toContain(
      '[&>.cost-strip]:hidden',
    )
  })

  it('keeps the cost strip visible once the library reports spend', () => {
    mocks.documents.libraryCost = 12.5
    const { container } = renderPage()
    expect(container.querySelector('.tab-panel > div')?.getAttribute('class')).toBeNull()
  })

  it('warns that upload is unavailable and routes to the binding screen', async () => {
    mocks.app.activeLibrary = buildLibrary({ ingestionReady: false })
    mocks.app.libraries = [buildLibrary({ ingestionReady: false })]
    renderPage()

    const warning = screen.getByRole('alert')
    expect(warning).toHaveTextContent('Document upload unavailable')
    expect(warning).toHaveTextContent(
      'AI bindings used by the document-processing workflow are not configured.',
    )
    expect(screen.getByRole('button', { name: 'Add content' })).toBeDisabled()

    await userEvent.setup().click(screen.getByRole('button', { name: 'Open AI Settings' }))
    expect(screen.getByRole('heading', { name: 'AI settings' })).toBeInTheDocument()
  })
})

describe('DocumentsPage web ingest tab', () => {
  beforeEach(() => {
    mocks.app.activeLibrary = buildLibrary()
    mocks.app.libraries = [buildLibrary()]
    mocks.app.user.role = 'operator'
  })

  it('swaps the header actions for the web run actions and refreshes runs', async () => {
    renderPage()

    await userEvent.setup().click(screen.getByRole('tab', { name: 'Web Ingest' }))
    expect(screen.getByRole('tab', { name: 'Web Ingest' })).toHaveAttribute('aria-selected', 'true')
    expect(screen.queryByRole('button', { name: 'Refresh' })).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Add link' })).toBeInTheDocument()
    expect(screen.getByText('web panel canWrite=true')).toBeInTheDocument()

    await userEvent.setup().click(screen.getByRole('button', { name: 'Refresh Runs' }))
    expect(mocks.web.refreshWebRuns).toHaveBeenCalledTimes(1)
  })
})

describe('DocumentsPage upload drop target', () => {
  beforeEach(() => {
    mocks.app.activeLibrary = buildLibrary()
    mocks.app.libraries = [buildLibrary()]
    mocks.documents = stubDocuments({ filteredTotal: 0, totalCount: 0 })
  })

  it('shows the drop overlay only while files are over a ready library', () => {
    const { container } = renderPage()
    const page = container.querySelector('.documents-page')
    expect(page).not.toBeNull()

    fireEvent.dragOver(page!)
    expect(screen.getByText('Drop files to upload')).toBeInTheDocument()

    fireEvent.dragLeave(page!)
    expect(screen.queryByText('Drop files to upload')).not.toBeInTheDocument()
  })

  it('opens the upload dialog with the dropped file count', () => {
    const { container } = renderPage()
    const page = container.querySelector('.documents-page')

    fireEvent.drop(page!, {
      dataTransfer: { files: [new File(['content'], 'handbook.txt', { type: 'text/plain' })] },
    })

    expect(screen.getByRole('dialog', { name: 'Upload documents' })).toBeInTheDocument()
    expect(screen.getByText('1 file selected')).toBeInTheDocument()
  })

  it('stops at the duplicate dialog and opens the existing document instead of uploading', async () => {
    mocks.documents = stubDocuments({
      filteredTotal: 1,
      items: [buildDocument()],
      totalCount: 1,
    })
    const { container } = renderPage()
    const page = container.querySelector('.documents-page')

    fireEvent.drop(page!, {
      dataTransfer: { files: [new File(['content'], 'handbook.txt', { type: 'text/plain' })] },
    })

    await userEvent.setup().click(screen.getByRole('button', { name: 'Upload' }))

    expect(screen.getByRole('dialog', { name: 'File already exists' })).toBeInTheDocument()
    expect(
      screen.getByText('A document named "handbook.txt" already exists in this library.'),
    ).toBeInTheDocument()

    await userEvent.setup().click(screen.getByRole('button', { name: 'Open existing' }))
    await waitFor(() =>
      expect(screen.queryByRole('dialog', { name: 'File already exists' })).not.toBeInTheDocument(),
    )
    expect(mocks.documents.loadFirstPage).toHaveBeenCalledTimes(1)
  })

  it('ignores a drop when the library cannot ingest', () => {
    mocks.app.activeLibrary = buildLibrary({ ingestionReady: false })
    mocks.app.libraries = [buildLibrary({ ingestionReady: false })]
    const { container } = renderPage()
    const page = container.querySelector('.documents-page')

    fireEvent.dragOver(page!)
    expect(screen.queryByText('Drop files to upload')).not.toBeInTheDocument()
  })
})
