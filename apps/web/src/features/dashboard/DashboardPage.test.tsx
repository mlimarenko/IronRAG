import { act } from 'react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { createRoot, type Root } from 'react-dom/client'
import { MemoryRouter, Route, Routes, useLocation } from 'react-router-dom'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import DashboardPage from '@/features/dashboard/DashboardPage'
import { Ops } from '@/shared/api'

const { useAppMock } = vi.hoisted(() => ({
  useAppMock: vi.fn(),
}))

vi.mock('@/shared/contexts/app-context', () => ({
  useApp: () => useAppMock(),
}))

// `useLibraryMetrics` resolves the dashboard payload through the
// generated TanStack hook (queries.getLibraryDashboardOptions), which
// in turn calls Ops.getLibraryDashboard. Mock the SDK class method so
// the real query/key plumbing exercises end-to-end and we only stub
// the network boundary.
vi.spyOn(Ops, 'getLibraryDashboard')

function LocationProbe() {
  const location = useLocation()
  return <div data-testid="destination">{`${location.pathname}${location.search}`}</div>
}

function sampleDashboard(overrides: Record<string, unknown> = {}) {
  const dashboard = {
    documentMetrics: {
      total: 12,
      ready: 8,
      processing: 1,
      queued: 1,
      failed: 1,
      canceled: 1,
      graphReady: 7,
      graphSparse: 1,
      recomputedAt: '2026-04-10T12:00:00Z',
    },
    recentDocuments: [
      {
        id: 'doc-active',
        fileName: 'active.pdf',
        fileSize: 2048,
        uploadedAt: new Date(Date.now() - 60_000).toISOString(),
        readiness: 'graph_ready',
        stageLabel: null,
        failureMessage: null,
        canRetry: false,
        preparedSegmentCount: 12,
        technicalFactCount: 4,
      },
      {
        id: 'doc-failed',
        fileName: 'broken.pdf',
        fileSize: 1024,
        uploadedAt: new Date(Date.now() - 120_000).toISOString(),
        readiness: 'failed',
        stageLabel: null,
        failureMessage: 'parser_error',
        canRetry: true,
        preparedSegmentCount: 0,
        technicalFactCount: 0,
      },
    ],
    recentWebRuns: [
      {
        runId: 'run-old',
        runState: 'completed',
        seedUrl: 'https://example.com/docs',
        counts: {
          discovered: 10,
          eligible: 10,
          processed: 8,
          queued: 0,
          processing: 0,
          blocked: 1,
          failed: 1,
        },
        lastActivityAt: '2026-04-09T09:00:00Z',
      },
      {
        runId: 'run-latest',
        runState: 'processing',
        seedUrl: 'https://example.com/api',
        counts: {
          discovered: 15,
          eligible: 15,
          processed: 5,
          queued: 3,
          processing: 2,
          blocked: 0,
          failed: 0,
        },
        lastActivityAt: '2026-04-10T09:00:00Z',
      },
    ],
    graph: {
      status: 'ready',
      warning: null,
      nodeCount: 42,
      edgeCount: 101,
      graphReadyDocumentCount: 7,
      graphSparseDocumentCount: 1,
      typedFactDocumentCount: 5,
      updatedAt: '2026-04-10T12:00:00Z',
    },
    attention: [
      {
        code: 'failed_documents',
        title: 'custom title ignored',
        detail: 'custom detail ignored',
        routePath: '/documents?status=failed',
        level: 'error',
      },
    ],
  }
  return { ...dashboard, ...overrides }
}

describe('DashboardPage integration', () => {
  let container: HTMLDivElement
  let root: Root | null

  const opsMock = vi.mocked(Ops.getLibraryDashboard)

  beforeEach(() => {
    vi.clearAllMocks()
    container = document.createElement('div')
    document.body.appendChild(container)
    root = null

    useAppMock.mockReturnValue({
      activeLibrary: { id: 'library-1', name: 'Main' },
    })
    // The hey-api fetch client envelope: `{ data, error, response }`. The
    // generated queryOptions strip out `data` for us; tests just need to
    // resolve the envelope.
    opsMock.mockResolvedValue({
      data: sampleDashboard(),
      error: undefined,
      response: new Response(),
      request: new Request('http://localhost'),
    } as never)
  })

  afterEach(async () => {
    if (root) {
      await act(async () => {
        root?.unmount()
      })
    }
    container.remove()
  })

  async function flushUi() {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0))
    })
  }

  async function renderPage() {
    const queryClient = new QueryClient({
      defaultOptions: {
        queries: {
          retry: false,
          staleTime: 0,
          refetchOnWindowFocus: false,
        },
      },
    })
    await act(async () => {
      root = createRoot(container)
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={['/']}>
            <Routes>
              <Route path="/" element={<DashboardPage />} />
              <Route path="/documents" element={<LocationProbe />} />
              <Route path="/graph" element={<LocationProbe />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      )
    })
    await flushUi()
    await flushUi()
  }

  function findButton(text: string) {
    return Array.from(container.querySelectorAll('button')).find((b) =>
      b.textContent?.includes(text),
    )
  }

  it('fetches the dashboard for the active library and renders summary tiles', async () => {
    await renderPage()

    expect(opsMock).toHaveBeenCalledTimes(1)
    expect(opsMock).toHaveBeenCalledWith(
      expect.objectContaining({ path: { libraryId: 'library-1' } }),
    )

    // Summary cards show derived counts, not raw backend values.
    expect(container.textContent).toContain('12') // total documents
    expect(container.textContent).toContain('58%') // 7/12 graph ready ≈ 58%
    expect(container.textContent).toContain('Active Operations')
  })

  it('localizes attention entries from their canonical code, not the backend title', async () => {
    await renderPage()

    // The backend sent `title: 'custom title ignored'` — the UI must NOT echo it
    // for known codes; it must use the translated `attentionTitles.failed_documents`.
    expect(container.textContent).not.toContain('custom title ignored')
    expect(container.textContent).toContain('Failed documents')
    expect(container.textContent).toContain('Review failed documents')
  })

  it('navigates failed-document attention to the failed documents filter', async () => {
    await renderPage()

    const attention = findButton('Review failed documents')
    expect(attention).toBeTruthy()

    await act(async () => {
      attention?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    await flushUi()

    expect(container.querySelector('[data-testid="destination"]')?.textContent).toBe(
      '/documents?status=failed',
    )
  })

  it('navigates graph attention to the graph workspace instead of documents', async () => {
    opsMock.mockResolvedValue({
      data: sampleDashboard({
        attention: [
          {
            code: 'graph_coverage_gap',
            title: 'ignored graph title',
            detail: 'ignored graph detail',
            routePath: '/graph',
            level: 'warning',
          },
        ],
      }),
      error: undefined,
      response: new Response(),
      request: new Request('http://localhost'),
    } as never)

    await renderPage()

    const attention = findButton('Open graph coverage')
    expect(attention).toBeTruthy()

    await act(async () => {
      attention?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    await flushUi()

    expect(container.querySelector('[data-testid="destination"]')?.textContent).toBe('/graph')
  })

  it('does not render duplicate generic document buttons on the dashboard', async () => {
    await renderPage()

    expect(findButton('Open Documents')).toBeUndefined()
  })

  it('surfaces the most recent web run, not the first in the list', async () => {
    await renderPage()

    // Latest run selection is by lastActivityAt desc, so `run-latest` wins.
    expect(container.textContent).toContain('example.com/api')
    expect(container.textContent).not.toContain('example.com/docs')
  })

  it('navigates to documents with the deep-link for a recent document card', async () => {
    await renderPage()

    const card = findButton('broken.pdf')
    expect(card).toBeTruthy()
    expect(card?.textContent).toContain('Processing failed: Parser error.')
    expect(card?.textContent).not.toContain('parser_error')

    await act(async () => {
      card?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    await flushUi()

    expect(container.querySelector('[data-testid="destination"]')?.textContent).toBe(
      '/documents?documentId=doc-failed',
    )
  })

  it('refreshes on demand without rebuilding the whole page', async () => {
    await renderPage()
    expect(opsMock).toHaveBeenCalledTimes(1)

    const refresh = Array.from(container.querySelectorAll('button')).find((b) =>
      b.textContent?.trim().toLowerCase().includes('refresh'),
    )
    expect(refresh).toBeTruthy()

    await act(async () => {
      refresh?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    await flushUi()
    await flushUi()

    expect(opsMock).toHaveBeenCalledTimes(2)
  })

  it('renders the no-library empty state when no active library is set', async () => {
    useAppMock.mockReturnValue({ activeLibrary: null })
    await renderPage()

    expect(opsMock).not.toHaveBeenCalled()
    expect(container.textContent).toContain('No library selected')
  })
})
