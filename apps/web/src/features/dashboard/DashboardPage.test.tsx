import { fireEvent, render, screen, within } from '@testing-library/react'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { MemoryRouter } from 'react-router-dom'
import { afterEach, describe, expect, it, vi } from 'vitest'

import DashboardPage from './DashboardPage'

const dashboardStyles = readFileSync(resolve(process.cwd(), 'src/styles/dashboard.css'), 'utf8')

const mocks = vi.hoisted(() => ({
  app: {
    activeLibrary: { id: 'library-1', name: 'Example library' },
    libraries: [{ id: 'library-1', name: 'Example library' }],
    locale: 'en',
  },
  queryResult: {
    data: {
      metrics: {
        total: 4,
        ready: 4,
        processing: 0,
        queued: 0,
        failed: 0,
        canceled: 0,
        empty: 0,
        graphReady: 2,
        graphSparse: 2,
        recomputedAt: '2026-08-09T09:18:00.000Z',
      },
      graph: {
        status: 'partial',
        nodes: 24,
        edges: 63,
        typedFactDocumentCount: 2,
      },
      attention: [] as Array<{
        code: string
        title: string
        detail: string
        routePath: string
        severity: 'info' | 'warning' | 'error'
      }>,
      recentDocuments: [] as Array<{
        id: string
        fileName: string | null
        fileType: string | null
        fileSize: number | null
        uploadedAt: string
        status: 'ready' | 'processing' | 'failed'
        readiness: 'graph_ready' | 'readable' | 'graph_sparse' | 'processing' | 'failed'
        stageLabel: string | null
        progressPercent: number | null
        failureMessage: string | null
        canRetry: boolean
      }>,
      lastWebRun: null as null | {
        state:
          | 'accepted'
          | 'discovering'
          | 'processing'
          | 'completed'
          | 'completed_partial'
          | 'failed'
          | 'canceled'
        processed: number
        queued: number
        lastActivityAt: string | null
      },
      warnings: [],
    },
    isLoading: false,
    isError: false,
    isFetching: false,
    refetch: vi.fn(),
  },
}))

vi.mock('@tanstack/react-query', () => ({
  useQuery: () => mocks.queryResult,
}))

vi.mock('react-i18next', () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}))

vi.mock('@/shared/contexts/app-context', () => ({
  useApp: () => mocks.app,
}))

// Rendered without the shell, so the preferences bridge has to be stood up for
// the empty-state picker that reads the admission role.
vi.mock('@/app/preferences-context', () => ({
  usePreferences: () => ({ role: 'admin' }),
}))

// Presentation only: the query result is handed in, and the stubbed options
// carry no queryFn, so nothing here can observe how the screen gets its data.
// Whether the query is dispatched at all is checked against the real query,
// client and wire in src/app/App.coldLoad.test.tsx.
vi.mock('@/shared/api', () => ({
  queries: {
    getLibraryDashboardOptions: () => ({ queryKey: ['dashboard'] }),
  },
}))

describe('DashboardPage knowledge coverage', () => {
  afterEach(() => {
    mocks.queryResult.data.metrics.total = 4
    mocks.queryResult.data.metrics.ready = 4
    mocks.queryResult.data.metrics.graphReady = 2
    mocks.queryResult.data.metrics.graphSparse = 2
    mocks.queryResult.data.metrics.failed = 0
    mocks.queryResult.data.metrics.canceled = 0
    mocks.queryResult.data.metrics.processing = 0
    mocks.queryResult.data.metrics.queued = 0
    mocks.queryResult.data.graph.status = 'partial'
    mocks.queryResult.data.attention = []
    mocks.queryResult.data.recentDocuments = []
    mocks.queryResult.data.lastWebRun = null
  })

  it('keeps the library heading and actions without a repeated introductory subtitle', () => {
    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Example library')
    expect(screen.getByRole('button', { name: 'common.refresh' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'dashboard.ask' })).toBeInTheDocument()
    expect(screen.queryByText('dashboard.subtitle')).not.toBeInTheDocument()
  })

  it('measures readiness across all 68 documents, not only the 43 with relationships', () => {
    Object.assign(mocks.queryResult.data.metrics, {
      total: 68,
      ready: 43,
      graphReady: 43,
      graphSparse: 0,
      processing: 8,
      queued: 17,
    })
    mocks.queryResult.data.graph.status = 'partial'
    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    const ring = document.querySelector('.coverage-orbit')
    expect(ring).toHaveAttribute('aria-valuenow', '63')
    expect(ring).toHaveAccessibleName('dashboard.documentReadinessCoverage')
    expect(ring).toHaveTextContent('63%')
    expect(screen.getByText('dashboard.factMapComplete')).toBeInTheDocument()
    expect(screen.queryByText('dashboard.factMapBuilding')).not.toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'dashboard.inFlightDocuments' })).toHaveAttribute(
      'href',
      '/documents?status=inflight',
    )
    for (const key of ['dashboard.readyForSearch', 'dashboard.factMapCompleteDocuments']) {
      expect(screen.getByRole('progressbar', { name: key })).toHaveAttribute('aria-valuenow', '63')
    }
    expect(
      screen.getByRole('progressbar', { name: 'dashboard.notReadyDocuments' }),
    ).toHaveAttribute('aria-valuenow', '37')
    expect(
      screen.getByRole('progressbar', { name: 'dashboard.factMapLimitedDocuments' }),
    ).toHaveAttribute('aria-valuenow', '0')
  })

  it('keeps graphless ready documents fully ready for answers', () => {
    mocks.queryResult.data.metrics.graphReady = 0
    mocks.queryResult.data.metrics.graphSparse = 4
    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )
    expect(
      screen.getByRole('progressbar', { name: 'dashboard.documentReadinessCoverage' }),
    ).toHaveAttribute('aria-valuenow', '100')
    expect(screen.getByText('dashboard.factMapIncomplete')).toBeInTheDocument()
  })

  it('accounts for every not-ready lifecycle bucket without calling queued work active', () => {
    mocks.queryResult.data.metrics.total = 11
    mocks.queryResult.data.metrics.processing = 2
    mocks.queryResult.data.metrics.queued = 3
    mocks.queryResult.data.metrics.failed = 1
    mocks.queryResult.data.metrics.canceled = 1

    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    const notReadyMetric = screen.getByRole('link', { name: /dashboard\.notReadyForSearch/ })
    expect(notReadyMetric).toHaveTextContent('7')
    // Only the buckets that hold something: a fixed four-part line spent its
    // width on zeros and wrapped to a second row to report them.
    expect(notReadyMetric).toHaveTextContent('dashboard.notReadyProcessing')
    expect(notReadyMetric).toHaveTextContent('dashboard.notReadyQueued')
    expect(notReadyMetric).toHaveTextContent('dashboard.notReadyFailed')
    expect(notReadyMetric).toHaveTextContent('dashboard.notReadyCanceled')
  })

  it('names only the not-ready buckets that hold something', () => {
    mocks.queryResult.data.metrics.total = 11
    mocks.queryResult.data.metrics.processing = 2
    mocks.queryResult.data.metrics.queued = 0
    mocks.queryResult.data.metrics.failed = 0
    mocks.queryResult.data.metrics.canceled = 0

    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    const notReadyMetric = screen.getByRole('link', { name: /dashboard\.notReadyForSearch/ })
    expect(notReadyMetric).toHaveTextContent('dashboard.notReadyProcessing')
    expect(notReadyMetric).not.toHaveTextContent('dashboard.notReadyQueued')
    expect(notReadyMetric).not.toHaveTextContent('dashboard.notReadyFailed')
    expect(notReadyMetric).not.toHaveTextContent('dashboard.notReadyCanceled')
    // A comma-joined list of statuses is not a filter the documents screen
    // parses: it silently fell back to the unfiltered list, so this card
    // counted seven materials and opened a page showing all hundred.
    expect(notReadyMetric).toHaveAttribute('href', '/documents?status=notready')
  })

  it('describes search and the optional fact map without projection terminology', () => {
    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    const heading = screen.getByRole('heading', { name: 'dashboard.memoryReadiness' })
    const panel = heading.closest('.panel')

    expect(panel).not.toBeNull()
    const coverage = within(panel as HTMLElement)
    expect(coverage.getByText('dashboard.memoryReadinessExplanation')).toBeInTheDocument()
    expect(coverage.getAllByText('dashboard.readyForSearch')).toHaveLength(2)
    expect(coverage.getByText('dashboard.factMapCompleteDocuments')).toBeInTheDocument()
    expect(coverage.getByText('dashboard.factMapLimitedDocuments')).toBeInTheDocument()
    expect(coverage.queryByText('dashboard.inActiveProjection')).not.toBeInTheDocument()
    expect(coverage.queryByText('dashboard.outsideActiveProjection')).not.toBeInTheDocument()
    expect(coverage.queryByText('dashboard.relationshipsBuilding')).not.toBeInTheDocument()

    expect(coverage.getByText('dashboard.technicalDetails')).toBeInTheDocument()
    expect(coverage.getByText('dashboard.factMapStatus')).toBeInTheDocument()
  })

  it('sends the missing-relations row to the documents that can be reprocessed, not to the map', () => {
    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    // Both document counts open their exact subsets rather than a graph
    // canvas or the superset of all ready documents.
    expect(screen.getByText('dashboard.factMapCompleteDocuments').closest('a')).toHaveAttribute(
      'href',
      '/documents?status=graphready',
    )
    expect(screen.getByText('dashboard.factMapLimitedDocuments').closest('a')).toHaveAttribute(
      'href',
      '/documents?status=graphgap',
    )
  })

  it('never falls back to the sidebar label for its own heading', () => {
    mocks.app.activeLibrary = null as unknown as { id: string; name: string }
    try {
      render(
        <MemoryRouter>
          <DashboardPage />
        </MemoryRouter>,
      )

      // The eyebrow already carries the section name and the sidebar entry now
      // uses the same words, so reusing `nav.dashboard` here printed the same
      // string twice in one header and never said what was missing.
      const header = screen.getByRole('heading', { level: 1 })
      expect(header).toHaveTextContent('dashboard.noLibrary')
      expect(header).not.toHaveTextContent('nav.dashboard')
    } finally {
      mocks.app.activeLibrary = { id: 'library-1', name: 'Example library' }
    }
  })

  it('leaves the not-ready tile inert when every document is ready', () => {
    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    // The filtered list is empty by construction at zero, so the tile stopped
    // advertising itself as a destination — and stopped rendering the empty
    // breakdown line that came with it.
    expect(screen.getByText('dashboard.notReadyForSearch').closest('a')).not.toBeInTheDocument()
    expect(screen.queryByText('dashboard.notReadyProcessing')).not.toBeInTheDocument()
  })

  it('leaves the attention tile inert and unlinked when nothing is raised', () => {
    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    // A link with the "open me" arrow, pointing at a panel that is already on
    // screen, is a control that does nothing. With no notice raised there is
    // nowhere to go, so the tile stops pretending to be a destination.
    expect(
      screen.queryByRole('link', { name: /dashboard\.attentionCount/ }),
    ).not.toBeInTheDocument()
    // "All clear" belongs in the notices panel, which says it in full. The
    // tile carrying it too printed one sentence twice on the same screen.
    expect(screen.getAllByText('dashboard.noNotices')).toHaveLength(1)
    expect(screen.queryByText('dashboard.attentionHint')).not.toBeInTheDocument()
  })

  it('sends the attention tile straight to the fix when exactly one notice is raised', () => {
    mocks.queryResult.data.attention = [
      {
        code: 'queue_not_progressing',
        title: 'queue',
        detail: 'queue detail',
        routePath: '/admin/queue',
        severity: 'error',
      },
    ]

    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    // One notice has one destination, and it is the screen that resolves it —
    // not an anchor to a list of one item sitting right below the tile.
    expect(screen.getByText('dashboard.attentionOpenOne').closest('a')).toHaveAttribute(
      'href',
      '/admin/queue',
    )
  })

  it('takes focus to the notice list when the tile points several notices at it', () => {
    mocks.queryResult.data.attention = [
      {
        code: 'failed_rebuilds',
        title: 'failed',
        detail: 'failed detail',
        routePath: '/documents?status=failed',
        severity: 'error',
      },
      {
        code: 'vector_index_gap',
        title: 'vectors',
        detail: 'vectors detail',
        routePath: '/documents?status=vectorgap',
        severity: 'warning',
      },
    ]

    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    const list = document.getElementById('dashboard-attention')
    expect(list).not.toBeNull()
    // The panel is already in the viewport, so the browser scrolls nowhere and
    // the click reads as dead unless something visibly moves.
    expect(list).toHaveAttribute('tabindex', '-1')
    expect(dashboardStyles).toContain('.attention-list:focus-visible')
  })

  it('drops the readiness badge from finished documents in the recent list', () => {
    mocks.queryResult.data.recentDocuments = [1, 2, 3].map((index) => ({
      id: `ready-${index}`,
      fileName: `ready-${index}.md`,
      fileType: 'md',
      fileSize: 2048,
      uploadedAt: '2026-08-09T09:00:00.000Z',
      status: 'ready' as const,
      readiness: 'graph_ready' as const,
      stageLabel: null,
      progressPercent: null,
      failureMessage: null,
      canRetry: false,
    }))

    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    // The same badge on every finished row said one thing six times and told
    // the reader nothing about any of them. Only a row that needs something
    // keeps a badge.
    const recent = within(document.querySelector('.recent-docs') as HTMLElement)
    expect(recent.queryByText('dashboard.readyForSearch')).not.toBeInTheDocument()
    expect(recent.getByText('ready-1.md')).toBeInTheDocument()
  })

  it('renders absent publication metadata differently from a zero-byte file', () => {
    const base = {
      uploadedAt: '2026-08-09T09:00:00.000Z',
      status: 'ready' as const,
      readiness: 'graph_ready' as const,
      stageLabel: null,
      progressPercent: null,
      failureMessage: null,
      canRetry: false,
    }
    mocks.queryResult.data.recentDocuments = [
      { ...base, id: 'unpublished', fileName: null, fileType: null, fileSize: null },
      { ...base, id: 'empty-file', fileName: 'empty.txt', fileType: 'text/plain', fileSize: 0 },
    ]
    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )
    const absent = screen.getByRole('link', { name: /common\.notAvailable/ })
    expect(absent).not.toHaveTextContent('0 B')
    expect(screen.getByRole('link', { name: /empty\.txt/ })).toHaveTextContent('0 B')
  })

  it('counts every API attention item without claiming background work is active', () => {
    mocks.queryResult.data.attention = [
      {
        code: 'failed_rebuilds',
        title: 'failed',
        detail: 'failed detail',
        routePath: '/documents?status=failed',
        severity: 'error',
      },
      {
        code: 'graph_coverage_gap',
        title: 'building',
        detail: 'building detail',
        routePath: '/graph',
        severity: 'warning',
      },
      {
        code: 'vector_index_gap',
        title: 'search status',
        detail: 'search status detail',
        routePath: '/documents?status=vectorgap',
        severity: 'warning',
      },
    ]

    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    const attentionMetric = screen.getByRole('link', { name: 'dashboard.attentionCount' })
    expect(attentionMetric).toHaveTextContent('3')
    expect(attentionMetric).toHaveAttribute('href', '#dashboard-attention')
    expect(screen.getByRole('heading', { name: 'dashboard.attention' })).toBeInTheDocument()
    expect(screen.queryByText('dashboard.backgroundWork')).not.toBeInTheDocument()
  })

  it('reports fact-map completeness without implying an active background job', () => {
    mocks.queryResult.data.graph.status = 'ready'
    mocks.queryResult.data.metrics.graphSparse = 2

    const { rerender } = render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    expect(screen.getByText('dashboard.factMapIncomplete')).toBeInTheDocument()
    expect(screen.queryByText('dashboard.factMapComplete')).not.toBeInTheDocument()
    expect(screen.queryByText('dashboard.enhancementsBuilding')).not.toBeInTheDocument()

    mocks.queryResult.data.metrics.graphSparse = 0
    mocks.queryResult.data.metrics.graphReady = 4
    rerender(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    expect(screen.getByText('dashboard.factMapComplete')).toBeInTheDocument()
  })

  it('does not call an empty fact map complete when no documents are ready', () => {
    mocks.queryResult.data.metrics.total = 2
    mocks.queryResult.data.metrics.ready = 0
    mocks.queryResult.data.metrics.graphReady = 0
    mocks.queryResult.data.metrics.graphSparse = 0
    mocks.queryResult.data.graph.status = 'empty'

    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    expect(screen.getByText('dashboard.factMapNotAvailable')).toBeInTheDocument()
    expect(screen.queryByText('dashboard.factMapComplete')).not.toBeInTheDocument()
  })

  it('shows an empty library without presenting a completion percentage', () => {
    Object.assign(mocks.queryResult.data.metrics, {
      total: 0,
      ready: 0,
      graphReady: 0,
      graphSparse: 0,
    })
    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )
    expect(screen.queryByRole('progressbar')).not.toBeInTheDocument()
    expect(screen.getByText('dashboard.noDocs')).toBeInTheDocument()
  })

  it('does not offer drilldowns into empty document buckets', () => {
    mocks.queryResult.data.metrics.graphReady = 4
    mocks.queryResult.data.metrics.graphSparse = 0
    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )
    for (const key of ['dashboard.notReadyDocuments', 'dashboard.factMapLimitedDocuments']) {
      expect(screen.getByText(key).closest('a')).toBeNull()
    }
    expect(screen.getByText('dashboard.factDocsSimple').closest('a')).toBeNull()
  })

  it('renders the real-world count split as reported and never labels sparse data as building', () => {
    mocks.queryResult.data.metrics.total = 14_638
    mocks.queryResult.data.metrics.ready = 14_632
    mocks.queryResult.data.metrics.graphReady = 1_342
    mocks.queryResult.data.metrics.graphSparse = 13_290
    mocks.queryResult.data.metrics.processing = 2
    mocks.queryResult.data.metrics.queued = 1
    mocks.queryResult.data.metrics.failed = 2
    mocks.queryResult.data.metrics.canceled = 1
    mocks.queryResult.data.graph.status = 'ready'
    mocks.queryResult.data.attention = [
      'failed_documents',
      'graph_coverage_gap',
      'queue_not_progressing',
      'vector_index_gap',
    ].map((code) => ({
      code,
      title: `${code} title`,
      detail: `${code} detail`,
      routePath: '/graph',
      severity: 'warning' as const,
    }))

    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    expect(screen.getByRole('link', { name: /dashboard\.materials/ })).toHaveTextContent('14,638')
    expect(screen.getByText('dashboard.factMapCompleteDocuments').closest('a')).toHaveTextContent(
      '1,342',
    )
    expect(screen.getByText('dashboard.factMapLimitedDocuments').closest('a')).toHaveTextContent(
      '13,290',
    )
    expect(
      screen.getByRole('progressbar', { name: 'dashboard.documentReadinessCoverage' }),
    ).toHaveAttribute('aria-valuenow', '99')
    expect(screen.getByRole('link', { name: 'dashboard.attentionCount' })).toHaveTextContent('4')
    expect(screen.getByRole('link', { name: /dashboard\.notReadyForSearch/ })).toHaveTextContent(
      '6',
    )
    expect(screen.queryByText('dashboard.relationshipsBuilding')).not.toBeInTheDocument()
    expect(screen.queryByText('dashboard.enhancementsBuilding')).not.toBeInTheDocument()
  })

  it('replaces raw processing and failure jargon with localized next steps', () => {
    mocks.queryResult.data.recentDocuments = [
      {
        id: 'processing-document',
        fileName: 'processing.pdf',
        fileType: 'pdf',
        fileSize: 2_048,
        uploadedAt: '2026-08-09T09:00:00.000Z',
        status: 'ready',
        readiness: 'processing',
        stageLabel: 'Embedding chunks',
        progressPercent: 35,
        failureMessage: null,
        canRetry: false,
      },
      {
        id: 'failed-document',
        fileName: 'failed.pdf',
        fileType: 'pdf',
        fileSize: 4_096,
        uploadedAt: '2026-08-09T08:00:00.000Z',
        status: 'ready',
        readiness: 'failed',
        stageLabel: null,
        progressPercent: null,
        failureMessage: 'Embedding failed',
        canRetry: true,
      },
    ]

    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    expect(screen.getByText('dashboard.preparingForSearch')).toBeInTheDocument()
    expect(
      screen.getByRole('progressbar', { name: 'dashboard.preparingForSearch' }),
    ).toBeInTheDocument()
    expect(screen.getByText('dashboard.failedDocumentRetry')).toBeInTheDocument()
    expect(screen.queryByText('Embedding chunks')).not.toBeInTheDocument()
    expect(screen.queryByText('Embedding failed')).not.toBeInTheDocument()
  })

  it('uses novice-safe localized copy for an unknown attention code', () => {
    mocks.queryResult.data.attention = [
      {
        code: 'future_backend_notice',
        title: 'Vector projection stalled',
        detail: 'Embedding chunks are stale.',
        routePath: '/documents',
        severity: 'warning',
      },
    ]

    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    expect(screen.getByText('dashboard.attentionItem.unknown.title')).toBeInTheDocument()
    expect(screen.getByText('dashboard.attentionItem.unknown.detail')).toBeInTheDocument()
    expect(screen.queryByText('Vector projection stalled')).not.toBeInTheDocument()
    expect(screen.queryByText('Embedding chunks are stale.')).not.toBeInTheDocument()
  })

  it.each([
    ['processing', true],
    ['completed', false],
    ['failed', false],
  ] as const)('shows the website-import live indicator for %s: %s', (state, expected) => {
    mocks.queryResult.data.lastWebRun = {
      state,
      processed: 3,
      queued: 0,
      lastActivityAt: null,
    }

    const { container } = render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    expect(container.querySelector('.web-run .live-dot') !== null).toBe(expected)
  })

  it.each([0, 10, 100])(
    'renders document readiness %s with matching visual and progress semantics',
    (ready) => {
      mocks.queryResult.data.metrics.ready = ready
      mocks.queryResult.data.metrics.total = 100
      mocks.queryResult.data.metrics.graphReady = 0
      mocks.queryResult.data.metrics.graphSparse = ready

      render(
        <MemoryRouter>
          <DashboardPage />
        </MemoryRouter>,
      )

      const donut = screen.getByRole('progressbar', {
        name: 'dashboard.documentReadinessCoverage',
      })
      expect(donut).toHaveAttribute('aria-valuemin', '0')
      expect(donut).toHaveAttribute('aria-valuemax', '100')
      expect(donut).toHaveAttribute('aria-valuenow', String(ready))
      expect(donut).toHaveStyle(`--coverage-percent: ${ready}%`)
      expect(donut).toHaveTextContent(`${ready}%`)
    },
  )

  it('uses the reported percentage as the completed conic-gradient sector', () => {
    expect(dashboardStyles).toContain('var(--ready) 0 var(--coverage-percent)')
    expect(dashboardStyles).toContain('var(--surface-3) var(--coverage-percent) 100%')
  })

  it('does not infer relationship construction from queued document ingestion', () => {
    mocks.queryResult.data.graph.status = 'partial'
    mocks.queryResult.data.metrics.graphSparse = 2
    mocks.queryResult.data.metrics.processing = 0
    mocks.queryResult.data.metrics.queued = 0

    const { rerender } = render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    expect(screen.getByText('dashboard.factMapIncomplete')).toBeInTheDocument()
    expect(screen.queryByText('dashboard.factMapBuilding')).not.toBeInTheDocument()

    mocks.queryResult.data.metrics.queued = 3
    rerender(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    expect(screen.queryByText('dashboard.factMapBuilding')).not.toBeInTheDocument()
    expect(screen.getByText('dashboard.factMapIncomplete')).toBeInTheDocument()
  })
})

describe('DashboardPage refresh behaviour', () => {
  afterEach(() => {
    vi.useRealTimers()
    mocks.queryResult.isError = false
    mocks.queryResult.refetch = vi.fn()
  })

  it('refetches on manual refresh and disables it while a request is in flight', () => {
    const { rerender } = render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )
    const refresh = screen.getByRole('button', { name: 'common.refresh' })
    fireEvent.click(refresh)
    expect(mocks.queryResult.refetch).toHaveBeenCalledTimes(1)
    mocks.queryResult.isFetching = true
    try {
      rerender(
        <MemoryRouter>
          <DashboardPage />
        </MemoryRouter>,
      )
      expect(refresh).toBeDisabled()
      fireEvent.click(refresh)
      expect(mocks.queryResult.refetch).toHaveBeenCalledTimes(1)
    } finally {
      mocks.queryResult.isFetching = false
    }
  })

  it('waits for the previous poll to answer before asking again', async () => {
    vi.useFakeTimers()
    let settle: (() => void) | undefined
    const refetch = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          settle = resolve
        }),
    )
    mocks.queryResult.refetch = refetch

    render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    await vi.advanceTimersByTimeAsync(2_500)
    expect(refetch).toHaveBeenCalledTimes(1)

    // The response is slower than the interval. A fixed interval would have
    // opened a second request on top of the first one by now.
    await vi.advanceTimersByTimeAsync(5_000)
    expect(refetch).toHaveBeenCalledTimes(1)

    settle?.()
    await vi.advanceTimersByTimeAsync(2_499)
    expect(refetch).toHaveBeenCalledTimes(1)

    await vi.advanceTimersByTimeAsync(1)
    expect(refetch).toHaveBeenCalledTimes(2)
  })

  it('skips its turn while the tab is hidden and resumes when it comes back', async () => {
    vi.useFakeTimers()
    const refetch = vi.fn(() => Promise.resolve())
    mocks.queryResult.refetch = refetch
    const visibility = vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden')

    try {
      render(
        <MemoryRouter>
          <DashboardPage />
        </MemoryRouter>,
      )

      await vi.advanceTimersByTimeAsync(10_000)
      expect(refetch).not.toHaveBeenCalled()

      visibility.mockReturnValue('visible')
      await vi.advanceTimersByTimeAsync(2_500)
      expect(refetch).toHaveBeenCalledTimes(1)
    } finally {
      visibility.mockRestore()
    }
  })

  it('keeps the loaded screen when a background poll fails and only errors with nothing to show', () => {
    mocks.queryResult.isError = true

    const { unmount } = render(
      <MemoryRouter>
        <DashboardPage />
      </MemoryRouter>,
    )

    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
    expect(screen.getByRole('status')).toHaveTextContent('dashboard.refreshFailed')
    expect(screen.getByText('dashboard.notReadyForSearch')).toBeInTheDocument()
    unmount()

    const loaded = mocks.queryResult.data
    mocks.queryResult.data = undefined as unknown as typeof loaded
    try {
      render(
        <MemoryRouter>
          <DashboardPage />
        </MemoryRouter>,
      )

      expect(screen.getByRole('alert')).toBeInTheDocument()
    } finally {
      mocks.queryResult.data = loaded
    }
  })
})
