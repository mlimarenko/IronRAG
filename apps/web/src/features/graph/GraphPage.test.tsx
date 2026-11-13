import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { MemoryRouter } from 'react-router-dom'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ScopeSwitcherContext } from '@/app/scope-switcher-context'
import { GraphPreparationError } from '@/shared/api/knowledge'
import i18n from '@/shared/i18n'
import GraphPage from './GraphPage'

const mocks = vi.hoisted(() => ({
  app: {
    activeLibrary: { id: 'library-1', name: 'Library' },
    libraries: [{ id: 'library-1', name: 'Library' }],
    locale: 'en',
  },
  getGraphTopology: vi.fn(),
  getSummary: vi.fn(),
  getEntity: vi.fn(),
  getDocument: vi.fn(),
}))

vi.mock('@/shared/contexts/app-context', () => ({
  useApp: () => mocks.app,
}))

// Rendered without the shell, so the preferences bridge has to be stood up for
// the empty-state picker that reads the admission role.
vi.mock('@/app/preferences-context', () => ({
  usePreferences: () => ({ role: 'admin' }),
}))

vi.mock('@/shared/api', () => ({
  knowledgeApi: { getGraphTopology: mocks.getGraphTopology },
  queries: {
    getKnowledgeLibrarySummaryOptions: ({ path }: { path: { libraryId: string } }) => ({
      queryKey: ['graph-summary', path.libraryId],
      queryFn: mocks.getSummary,
    }),
    getKnowledgeEntityOptions: ({ path }: { path: { libraryId: string; entityId: string } }) => ({
      queryKey: ['graph-entity', path.libraryId, path.entityId],
      queryFn: () => mocks.getEntity(path.entityId),
    }),
    getContentDocumentOptions: ({ path }: { path: { documentId: string } }) => ({
      queryKey: ['graph-document', path.documentId],
      queryFn: () => mocks.getDocument(path.documentId),
    }),
  },
}))

vi.mock('./GraphCanvas', () => ({
  GraphCanvas: ({
    nodes,
    layout,
  }: {
    nodes: Array<{ id: string; label: string }>
    layout: string
  }) => (
    <div data-testid="graph-canvas" data-layout={layout}>
      {nodes.map((node) => node.label).join(', ')}
    </div>
  ),
}))

function renderGraph(initialEntry = '/graph') {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false } },
  })

  function Wrapper({ children }: Readonly<{ children: ReactNode }>) {
    return (
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={[initialEntry]}>{children}</MemoryRouter>
      </QueryClientProvider>
    )
  }

  return render(<GraphPage />, { wrapper: Wrapper })
}

const readySummary = {
  documentCountsByReadiness: {},
  edgeCount: 1,
  graphReadyDocumentCount: 1,
  graphSparseDocumentCount: 0,
  latestGeneration: {
    id: 'generation-1',
    library_id: 'library-1',
    generation_kind: 'library',
    generation_state: 'graph_ready',
    created_at: '2026-08-09T00:00:00Z',
  },
  libraryId: 'library-1',
  nodeCount: 2,
  typedFactDocumentCount: 1,
  updatedAt: '2026-08-09T00:00:00Z',
}

const topology = {
  documents: [{ documentId: 'document-1', title: 'Runbook' }],
  entities: [
    {
      entityId: 'entity-1',
      canonicalLabel: 'Pipeline',
      entityType: 'concept',
      supportCount: 2,
      entityState: 'active',
      aliases: [],
    },
  ],
  relations: [],
  documentLinks: [{ documentId: 'document-1', targetNodeId: 'entity-1', supportCount: 1 }],
}

describe('GraphPage', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    mocks.app.activeLibrary = { id: 'library-1', name: 'Library' }
    mocks.getSummary.mockResolvedValue(readySummary)
    mocks.getGraphTopology.mockResolvedValue(topology)
    mocks.getEntity.mockResolvedValue({
      entity: {
        entityId: 'entity-1',
        canonicalLabel: 'Pipeline',
        entityType: 'concept',
        entityState: 'active',
        supportCount: 2,
        aliases: [],
      },
      supportingEvidence: [],
    })
    mocks.getDocument.mockResolvedValue({})
  })

  it('shows node progress reported from the stream meta frame', async () => {
    mocks.getSummary.mockResolvedValue({ ...readySummary, nodeCount: 0, latestGeneration: null })
    mocks.getGraphTopology.mockImplementation(
      async (_libraryId: string, options: { onProgress?: (progress: unknown) => void }) => {
        options.onProgress?.({
          nodesLoaded: 3,
          edgesLoaded: 1,
          expectedNodes: 10,
          expectedEdges: 4,
        })
        return await new Promise(() => undefined)
      },
    )

    renderGraph()

    expect(await screen.findByText(/3 \/ 10 nodes/)).toBeInTheDocument()
  })

  it('aborts the previous library request and the active request on unmount', async () => {
    mocks.getGraphTopology.mockImplementation(() => new Promise(() => undefined))
    const view = renderGraph()
    await waitFor(() => expect(mocks.getGraphTopology).toHaveBeenCalledTimes(1))
    const first = mocks.getGraphTopology.mock.calls[0]![1].signal as AbortSignal
    expect(first.aborted).toBe(false)
    mocks.app.activeLibrary = { id: 'library-2', name: 'Other' }
    view.rerender(<GraphPage />)
    await waitFor(() => expect(mocks.getGraphTopology).toHaveBeenCalledTimes(2))
    expect(first.aborted).toBe(true)
    const second = mocks.getGraphTopology.mock.calls[1]![1].signal as AbortSignal
    expect(second.aborted).toBe(false)
    view.unmount()
    expect(second.aborted).toBe(true)
  })

  it('does not install a result from the previous library after navigation', async () => {
    let resolvePrevious!: (value: typeof topology) => void
    mocks.getGraphTopology.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolvePrevious = resolve
        }),
    )
    const view = renderGraph()
    await waitFor(() => expect(mocks.getGraphTopology).toHaveBeenCalledTimes(1))
    mocks.app.activeLibrary = { id: 'library-2', name: 'Other' }
    view.rerender(<GraphPage />)
    expect(await screen.findByTestId('graph-canvas')).toHaveTextContent('Pipeline')
    await act(async () =>
      resolvePrevious({
        ...topology,
        entities: [{ ...topology.entities[0]!, canonicalLabel: 'Obsolete' }],
      }),
    )
    expect(screen.getByTestId('graph-canvas')).not.toHaveTextContent('Obsolete')
  })

  it('shows a terminal preparation error and retries only on request', async () => {
    mocks.getGraphTopology.mockRejectedValueOnce(new GraphPreparationError('expired'))
    renderGraph()
    expect(await screen.findByRole('alert')).toHaveTextContent('Graph preparation expired')
    expect(mocks.getGraphTopology).toHaveBeenCalledTimes(1)
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }))
    expect(await screen.findByTestId('graph-canvas')).toHaveTextContent('Pipeline')
    expect(mocks.getGraphTopology).toHaveBeenCalledTimes(2)
  })

  it('shows a localized failure rather than a parser error', async () => {
    await i18n.changeLanguage('ru')
    try {
      mocks.getGraphTopology.mockRejectedValueOnce(new Error('Invalid graph terminal counts'))
      renderGraph()
      const alert = await screen.findByRole('alert')
      expect(alert).toHaveTextContent('Не удалось загрузить граф')
      expect(alert).not.toHaveTextContent('Invalid graph terminal counts')
    } finally {
      await i18n.changeLanguage('en')
    }
  })

  it('opens a deep-linked entity and fetches its real detail', async () => {
    renderGraph('/graph?nodeId=entity-1')

    expect(await screen.findByTestId('graph-canvas')).toHaveTextContent('Pipeline')
    await waitFor(() => expect(mocks.getEntity).toHaveBeenCalledWith('entity-1'))
    expect(await screen.findByRole('complementary', { name: 'Selected node' })).toHaveTextContent(
      'Pipeline',
    )
  })

  it('closes the selected-node inspector with Escape without a dimming dialog', async () => {
    renderGraph('/graph?nodeId=entity-1')

    expect(await screen.findByRole('complementary', { name: 'Selected node' })).toBeInTheDocument()
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))
    await waitFor(() => {
      expect(screen.queryByRole('complementary', { name: 'Selected node' })).toBeNull()
    })
  })

  it('uses the force-directed layout by default and allows a manual override', async () => {
    renderGraph()

    const canvas = await screen.findByTestId('graph-canvas')
    expect(canvas).toHaveAttribute('data-layout', 'force')

    fireEvent.change(screen.getByRole('combobox'), { target: { value: 'hubs' } })
    expect(canvas).toHaveAttribute('data-layout', 'hubs')
  })

  it('does not mark an empty memory with a success tick', async () => {
    mocks.getSummary.mockResolvedValue({ ...readySummary, nodeCount: 0, edgeCount: 0 })
    mocks.getGraphTopology.mockResolvedValue({
      documents: [],
      entities: [],
      relations: [],
      documentLinks: [],
    })

    const { container } = renderGraph()

    expect(await screen.findByText('No connections yet')).toBeInTheDocument()
    // A checkmark beside "nothing here yet" reads as "all good" on a screen
    // that has nothing to show.
    const card = container.querySelector('.state-card')
    expect(card?.querySelector('svg.lucide-check')).toBeNull()
    expect(card?.querySelector('svg')).not.toBeNull()
  })
})

describe('GraphPage without a selected library', () => {
  beforeEach(() => {
    mocks.app.activeLibrary = null as unknown as { id: string; name: string }
  })

  afterEach(() => {
    mocks.app.activeLibrary = { id: 'library-1', name: 'Library' }
  })

  it('offers the picker that resolves the state instead of only naming it', () => {
    const opener = vi.fn()

    render(
      <MemoryRouter>
        <ScopeSwitcherContext.Provider value={{ openScopeSwitcher: opener }}>
          <GraphPage />
        </ScopeSwitcherContext.Provider>
      </MemoryRouter>,
    )

    // Telling a first-time reader to "select a library" while showing nothing
    // that selects one is a dead end: the picker lives in the shell top bar,
    // and this screen could not reach it.
    const choose = screen.getByRole('button', { name: 'Choose a library' })
    fireEvent.click(choose)
    expect(opener).toHaveBeenCalledTimes(1)
  })

  it('offers no picker it cannot open when rendered outside the shell', () => {
    render(
      <MemoryRouter>
        <GraphPage />
      </MemoryRouter>,
    )

    expect(screen.getByText('No library selected')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Choose a library' })).not.toBeInTheDocument()
  })
})
