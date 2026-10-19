import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen } from '@testing-library/react'
import type { ReactNode } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { WebIngestRunListItem, WebIngestRunPageItem } from '@/shared/api'
import i18n from '@/shared/i18n'

import { WebRunDetails } from './WebRunDetails'

const mocks = vi.hoisted(() => ({
  getRun: vi.fn(),
  listPages: vi.fn(),
}))

vi.mock('@/shared/api', () => ({
  queries: {
    getContentWebIngestRunOptions: ({ path }: { path: { runId: string } }) => ({
      queryFn: () => mocks.getRun(path.runId),
      queryKey: ['web-run', path.runId],
    }),
    listContentWebIngestRunPagesOptions: ({ path }: { path: { runId: string } }) => ({
      queryFn: () => mocks.listPages(path.runId),
      queryKey: ['web-run-pages', path.runId],
    }),
  },
}))

const run: WebIngestRunListItem = {
  boundaryPolicy: 'same_host',
  counts: {
    blocked: 0,
    canceled: 0,
    discovered: 1,
    duplicates: 0,
    eligible: 1,
    excluded: 0,
    failed: 0,
    processed: 1,
    processing: 0,
    queued: 0,
  },
  crawlFilter: { allowPatterns: [], blockPatterns: [] },
  lastActivityAt: '2026-08-11T08:00:00Z',
  libraryId: 'library-1',
  materializationFilter: { allowPatterns: [], blockPatterns: [] },
  maxDepth: 3,
  maxPages: 100,
  mode: 'recursive_crawl',
  runId: 'run-1',
  runState: 'completed',
  seedUrl: 'https://example.test/docs',
}

function page(overrides: Partial<WebIngestRunPageItem> = {}): WebIngestRunPageItem {
  return {
    candidateId: 'candidate-1',
    candidateState: 'excluded',
    classificationReason: 'url_filter',
    depth: 1,
    discoveredAt: '2026-08-11T07:59:00Z',
    hostClassification: 'same_host',
    normalizedUrl: 'https://example.test/docs/private',
    runId: run.runId,
    updatedAt: '2026-08-11T08:00:00Z',
    ...overrides,
  }
}

function renderDetails() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })

  function Wrapper({ children }: Readonly<{ children: ReactNode }>) {
    return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
  }

  return render(<WebRunDetails errorMessage={() => 'Unable to load'} run={run} />, {
    wrapper: Wrapper,
  })
}

describe('WebRunDetails', () => {
  beforeEach(async () => {
    vi.clearAllMocks()
    await i18n.changeLanguage('en')
    i18n.addResource('en', 'translation', 'documents.webRunDetailsTitle', 'Web run details')
    i18n.addResource('en', 'translation', 'documents.pageUrlUnavailable', 'Page URL unavailable')
    mocks.getRun.mockResolvedValue(run)
    mocks.listPages.mockResolvedValue([page()])
  })

  it('gives the expanded details panel its own heading', async () => {
    renderDetails()

    expect(await screen.findByRole('heading', { name: 'Web run details' })).toBeInTheDocument()
  })

  it('does not expose internal classification detail to the operator', async () => {
    mocks.listPages.mockResolvedValue([
      page({ classificationDetail: 'crawl:blocklist:prefix:/private' }),
    ])
    renderDetails()

    expect(await screen.findByText('Excluded by URL filter')).toBeInTheDocument()
    expect(screen.queryByText('crawl:blocklist:prefix:/private')).not.toBeInTheDocument()
  })

  it('explains why a page without a URL cannot be opened', async () => {
    mocks.listPages.mockResolvedValue([page({ normalizedUrl: '' })])
    renderDetails()

    const openButton = await screen.findByRole('button', { name: 'Open page' })

    expect(openButton).toBeDisabled()
    expect(openButton).toHaveAccessibleDescription('Page URL unavailable')
    expect(screen.getByText('Page URL unavailable')).toBeInTheDocument()
  })

  it('collapses the details controls and funnel below 768px', async () => {
    const { container } = renderDetails()

    await screen.findByText('https://example.test/docs/private')

    expect(container.querySelector('.funnel')).toHaveClass('max-md:grid-cols-1!')
    expect(container.querySelector('.candidate-toolbar')).toHaveClass(
      'max-md:flex-col!',
      'max-md:items-stretch!',
    )
    expect(container.querySelector('.candidate-toolbar > select')).toHaveClass('max-md:w-full!')
  })
})
