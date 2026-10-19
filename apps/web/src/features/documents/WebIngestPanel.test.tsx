import { render } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import type { WebIngestRunListItem } from '@/shared/api'

import type { WebIngestController } from './components/documents-page/useWebIngestController'
import { WebIngestPanel } from './WebIngestPanel'

vi.mock('@/shared/contexts/app-context', () => ({
  useApp: () => ({ locale: 'en' }),
}))

vi.mock('./components/documents-page/WebIngestDialog', () => ({
  WebIngestDialog: () => null,
}))

vi.mock('./components/documents-page/WebRunDetails', () => ({
  WebRunDetails: () => null,
}))

const run: WebIngestRunListItem = {
  boundaryPolicy: 'same_host',
  counts: {
    blocked: 0,
    canceled: 0,
    discovered: 4,
    duplicates: 0,
    eligible: 4,
    excluded: 0,
    failed: 0,
    processed: 2,
    processing: 2,
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
  runState: 'processing',
  seedUrl: 'https://example.test/docs',
}

function controllerWithRuns(webRuns: WebIngestRunListItem[]): WebIngestController {
  return {
    errorMessage: vi.fn(),
    openCreateDialog: vi.fn(),
    webRuns,
  } as unknown as WebIngestController
}

describe('WebIngestPanel', () => {
  it('stacks the run heading and actions below the medium breakpoint', () => {
    const { container } = render(
      <WebIngestPanel controller={controllerWithRuns([run])} canWrite={false} />,
    )

    const heading = container.querySelector('.run-card__heading')
    const actions = heading?.querySelector(':scope > div')

    expect(heading).toHaveClass('max-md:flex-col!')
    expect(actions).toHaveClass('max-md:w-full', 'max-md:flex-wrap')
  })
})
