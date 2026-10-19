import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import type { ReactNode } from 'react'
import { MemoryRouter } from 'react-router-dom'
import type * as ReactRouterDom from 'react-router-dom'
import { describe, expect, it, vi, beforeEach } from 'vitest'
import type { GraphNode } from '@/shared/types'
import type { GraphAdjacencyIndex } from '../hooks/useGraphAdjacency'
import { GraphInspector } from './GraphInspector'

const mocks = vi.hoisted(() => ({
  getDocument: vi.fn(),
  getEntity: vi.fn(),
  navigate: vi.fn(),
}))

vi.mock('@/shared/api', () => ({
  queries: {
    getContentDocumentOptions: ({ path }: { path: { documentId: string } }) => ({
      queryKey: ['graph-document', path.documentId],
      queryFn: () => mocks.getDocument(path.documentId),
    }),
    getKnowledgeEntityOptions: ({ path }: { path: { libraryId: string; entityId: string } }) => ({
      queryKey: ['graph-entity', path.libraryId, path.entityId],
      queryFn: () => mocks.getEntity(path.entityId),
    }),
  },
}))

vi.mock('react-router-dom', async () => {
  const actual = await vi.importActual<typeof ReactRouterDom>('react-router-dom')
  return { ...actual, useNavigate: () => mocks.navigate }
})

function emptyAdjacency(node: GraphNode): GraphAdjacencyIndex {
  return {
    neighborIds: new Map(),
    nodeById: new Map([[node.id, node]]),
    neighborhoodOf: () => ({ ids: [], nodes: [] }),
  }
}

function renderInspector(
  selectedBasic: GraphNode,
  adjacency: GraphAdjacencyIndex,
  extras: { neighborhoodFocused?: boolean; onFocusNeighborhood?: () => void } = {},
) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false } },
  })

  function Wrapper({ children }: Readonly<{ children: ReactNode }>) {
    return (
      <QueryClientProvider client={queryClient}>
        <MemoryRouter>{children}</MemoryRouter>
      </QueryClientProvider>
    )
  }

  return render(
    <GraphInspector
      libraryId="library-1"
      selectedBasic={selectedBasic}
      adjacency={adjacency}
      neighborhoodFocused={extras.neighborhoodFocused ?? false}
      onClose={vi.fn()}
      onSelectNode={vi.fn()}
      onFocusNeighborhood={extras.onFocusNeighborhood ?? vi.fn()}
    />,
    { wrapper: Wrapper },
  )
}

describe('GraphInspector', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })
  it('does not expose an internal document ID when a source label is unavailable', async () => {
    const sourceDocumentId = '8f7e6d5c-4b3a-2910-8877-665544332211'
    const selectedBasic: GraphNode = {
      id: 'entity-1',
      label: 'Synthetic entity',
      type: 'concept',
      edgeCount: 0,
      properties: {},
      sourceDocumentIds: [sourceDocumentId],
    }
    mocks.getEntity.mockResolvedValue({
      entity: {
        entityId: selectedBasic.id,
        canonicalLabel: selectedBasic.label,
        entityType: selectedBasic.type,
        entityState: 'active',
        aliases: [],
        supportCount: 1,
      },
      supportingEvidence: [{ documentId: sourceDocumentId }],
    })

    renderInspector(selectedBasic, emptyAdjacency(selectedBasic))

    const sourceButton = await screen.findByRole('button', { name: /Document/ })
    expect(sourceButton).not.toHaveAccessibleName(new RegExp(sourceDocumentId.slice(0, 8)))
  })

  it('docks as a complementary panel instead of a modal dialog', () => {
    const selectedBasic: GraphNode = {
      id: 'entity-1',
      label: 'Pipeline',
      type: 'concept',
      edgeCount: 0,
      properties: {},
    }
    mocks.getEntity.mockResolvedValue({
      entity: {
        entityId: selectedBasic.id,
        canonicalLabel: selectedBasic.label,
        entityType: selectedBasic.type,
        entityState: 'active',
        aliases: [],
        supportCount: 1,
      },
      supportingEvidence: [],
    })

    renderInspector(selectedBasic, emptyAdjacency(selectedBasic))

    expect(screen.getByRole('complementary', { name: 'Selected node' })).toBeInTheDocument()
    expect(screen.queryByRole('dialog')).toBeNull()
    const ask = screen.getByRole('button', { name: 'Ask about this' })
    const connections = screen.getByText('Connections')
    expect(ask.compareDocumentPosition(connections) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy()
  })

  it('does not send an entity to the unfiltered documents list', () => {
    const selectedBasic: GraphNode = {
      id: 'entity-1',
      label: 'Pipeline',
      type: 'concept',
      edgeCount: 0,
      properties: {},
    }
    mocks.getEntity.mockResolvedValue({
      entity: {
        entityId: selectedBasic.id,
        canonicalLabel: selectedBasic.label,
        entityType: selectedBasic.type,
        entityState: 'active',
        aliases: [],
        supportCount: 1,
      },
      supportingEvidence: [],
    })

    renderInspector(selectedBasic, emptyAdjacency(selectedBasic))

    expect(screen.queryByRole('button', { name: 'Go to materials' })).toBeNull()
    expect(screen.queryByRole('button', { name: 'Go to Documents' })).toBeNull()
  })

  it('opens the selected document and asks about it with a grounded prompt', async () => {
    const user = userEvent.setup()
    const selectedBasic: GraphNode = {
      id: 'document-1',
      label: 'Runbook',
      type: 'document',
      edgeCount: 0,
      properties: {},
    }
    mocks.getDocument.mockResolvedValue({
      document: {
        documentId: selectedBasic.id,
        title: selectedBasic.label,
        documentState: 'ready',
      },
    })

    renderInspector(selectedBasic, emptyAdjacency(selectedBasic))

    await user.click(screen.getByRole('button', { name: 'Open document' }))
    expect(mocks.navigate).toHaveBeenCalledWith('/documents?documentId=document-1')

    await user.click(screen.getByRole('button', { name: 'Ask about this' }))
    const promptCall = mocks.navigate.mock.calls.find((call) =>
      String(call[0]).startsWith('/assistant?prompt='),
    )
    expect(promptCall?.[0]).toContain(encodeURIComponent('Runbook'))
    expect(String(promptCall?.[0])).not.toContain(encodeURIComponent('Ask about this:'))
  })

  it('treats neighborhood focus as a pressed toggle on the neighbors section', async () => {
    const user = userEvent.setup()
    const onFocusNeighborhood = vi.fn()
    const selectedBasic: GraphNode = {
      id: 'entity-1',
      label: 'Pipeline',
      type: 'concept',
      edgeCount: 1,
      properties: {},
    }
    const neighbor: GraphNode = {
      id: 'entity-2',
      label: 'Valve',
      type: 'concept',
      edgeCount: 1,
      properties: {},
    }
    mocks.getEntity.mockResolvedValue({
      entity: {
        entityId: selectedBasic.id,
        canonicalLabel: selectedBasic.label,
        entityType: selectedBasic.type,
        entityState: 'active',
        aliases: [],
        supportCount: 1,
      },
      supportingEvidence: [],
    })

    renderInspector(
      selectedBasic,
      {
        neighborIds: new Map([[selectedBasic.id, [neighbor.id]]]),
        nodeById: new Map([
          [selectedBasic.id, selectedBasic],
          [neighbor.id, neighbor],
        ]),
        neighborhoodOf: () => ({ ids: [neighbor.id], nodes: [neighbor] }),
      },
      {
        neighborhoodFocused: true,
        onFocusNeighborhood,
      },
    )

    const toggle = screen.getByRole('button', { name: 'Show full graph' })
    expect(toggle).toHaveAttribute('aria-pressed', 'true')
    await user.click(toggle)
    expect(onFocusNeighborhood).toHaveBeenCalledWith('entity-1')
  })
})
