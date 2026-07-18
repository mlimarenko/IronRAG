import { act } from 'react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'
import type { WebIngestRunListItem } from '@/shared/api'
import { WebRunsPanel } from '@/features/documents/WebRunsPanel'

const { documentsApiMock, toastErrorMock } = vi.hoisted(() => ({
  documentsApiMock: {
    listWebRunPages: vi.fn(),
  },
  toastErrorMock: vi.fn(),
}))

vi.mock('sonner', () => ({
  toast: {
    error: toastErrorMock,
    success: vi.fn(),
  },
}))

vi.mock('@/shared/api', () => ({
  documentsApi: documentsApiMock,
  queries: {
    listContentWebIngestRunPagesOptions: (input: { path: { runId: string } }) => ({
      queryKey: ['mockedWebIngestRunPages', input.path.runId],
      queryFn: async () => documentsApiMock.listWebRunPages(input.path.runId),
    }),
  },
}))

describe('WebRunsPanel', () => {
  let container: HTMLDivElement
  let root: Root | null

  beforeEach(() => {
    toastErrorMock.mockClear()
    container = document.createElement('div')
    document.body.appendChild(container)
    root = null
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

  async function renderPanel() {
    const runs: WebIngestRunListItem[] = Array.from({ length: 12 }, (_, index) => ({
      runId: `run-${index + 1}`,
      libraryId: 'lib-1',
      seedUrl: `https://docs.example.com/run-${index + 1}`,
      runState: index === 0 ? 'processing' : 'completed',
      mode: 'recursive_crawl',
      boundaryPolicy: 'same_host',
      maxDepth: 3,
      maxPages: 250,
      crawlFilter: { allowPatterns: [], blockPatterns: [] },
      materializationFilter: { allowPatterns: [], blockPatterns: [] },
      counts: {
        discovered: 250,
        processed: index === 0 ? 120 : 250,
        failed: index === 0 ? 3 : 0,
        blocked: 0,
        canceled: 0,
        duplicates: 0,
        eligible: 0,
        excluded: 0,
        processing: 0,
        queued: 0,
      },
    }))

    const pages = Array.from({ length: 250 }, (_, index) => ({
      candidateId: `candidate-${index + 1}`,
      runId: 'run-1',
      normalizedUrl: `https://docs.example.com/page-${String(index + 1).padStart(3, '0')}`,
      candidateState: index === 1 ? 'materialized' : index % 9 === 0 ? 'failed' : 'processed',
      depth: 2,
      httpStatus: 200,
    }))

    documentsApiMock.listWebRunPages.mockResolvedValue(pages)

    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: 0, refetchOnWindowFocus: false } },
    })

    await act(async () => {
      root = createRoot(container)
      root.render(
        <QueryClientProvider client={queryClient}>
          <WebRunsPanel
            t={i18n.t.bind(i18n)}
            webRuns={runs}
            onReuseRun={() => {}}
            onCancelRun={() => Promise.resolve()}
          />
        </QueryClientProvider>,
      )
    })

    await flushUi()
    await flushUi()
  }

  function findButton(text: string) {
    return Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes(text),
    )
  }

  it('reports cancellation failures to the user', async () => {
    const onCancelRun = vi.fn().mockRejectedValue(new Error('request failed'))
    const run: WebIngestRunListItem = {
      runId: 'active-run',
      libraryId: 'lib-1',
      seedUrl: 'https://docs.example.com/active',
      runState: 'processing',
      mode: 'recursive_crawl',
      boundaryPolicy: 'same_host',
      maxDepth: 3,
      maxPages: 250,
      crawlFilter: { allowPatterns: [], blockPatterns: [] },
      materializationFilter: { allowPatterns: [], blockPatterns: [] },
      counts: {
        discovered: 1,
        processed: 0,
        failed: 0,
        blocked: 0,
        canceled: 0,
        duplicates: 0,
        eligible: 0,
        excluded: 0,
        processing: 1,
        queued: 0,
      },
    }
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } })

    await act(async () => {
      root = createRoot(container)
      root.render(
        <QueryClientProvider client={queryClient}>
          <WebRunsPanel
            t={i18n.t.bind(i18n)}
            webRuns={[run]}
            onReuseRun={() => {}}
            onCancelRun={onCancelRun}
          />
        </QueryClientProvider>,
      )
    })

    const cancelButton = container.querySelector(
      `button[aria-label="${i18n.t('documents.cancelRun')}"]`,
    )
    expect(cancelButton).toBeTruthy()
    await act(async () => {
      cancelButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    await flushUi()

    expect(onCancelRun).toHaveBeenCalledWith('active-run')
    expect(toastErrorMock).toHaveBeenCalledWith(i18n.t('documents.webIngestCancelFailed'))
  })

  it('renders runs beyond the first ten and paginates long page lists', async () => {
    await renderPanel()

    expect(container.textContent).toContain('https://docs.example.com/run-12')

    const firstRunButton = findButton('https://docs.example.com/run-1')
    expect(firstRunButton).toBeTruthy()

    await act(async () => {
      firstRunButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    await flushUi()
    await flushUi()

    expect(documentsApiMock.listWebRunPages).toHaveBeenCalledWith('run-1')
    expect(container.textContent).toContain('page-001')
    expect(container.textContent).toContain('Awaiting publication')
    expect(container.textContent).toContain('1–200 of 250 URLs')
    expect(container.textContent).not.toContain('page-225')

    const nextButton = findButton('Next')
    expect(nextButton).toBeTruthy()

    await act(async () => {
      nextButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    await flushUi()

    expect(container.textContent).toContain('201–250 of 250 URLs')
    expect(container.textContent).toContain('page-225')
  })

  it('reports a failed run cancellation instead of leaving a rejected interaction', async () => {
    const onCancelRun = vi.fn().mockRejectedValue(new Error('cancellation unavailable'))
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: 0 } },
    })

    await act(async () => {
      root = createRoot(container)
      root.render(
        <QueryClientProvider client={queryClient}>
          <WebRunsPanel
            t={i18n.t.bind(i18n)}
            webRuns={[
              {
                runId: 'run-cancel',
                libraryId: 'lib-1',
                seedUrl: 'https://example.test',
                runState: 'processing',
                mode: 'single_page',
                boundaryPolicy: 'same_host',
                maxDepth: 1,
                maxPages: 1,
                crawlFilter: { allowPatterns: [], blockPatterns: [] },
                materializationFilter: { allowPatterns: [], blockPatterns: [] },
              },
            ]}
            onReuseRun={() => {}}
            onCancelRun={onCancelRun}
          />
        </QueryClientProvider>,
      )
    })

    const cancelButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.getAttribute('aria-label') === i18n.t('documents.cancelRun'),
    )
    await act(async () => {
      cancelButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    await flushUi()

    expect(onCancelRun).toHaveBeenCalledWith('run-cancel')
    expect(toastErrorMock).toHaveBeenCalledWith(i18n.t('documents.webIngestCancelFailed'))
  })

  it('reports clipboard failures to the operator', async () => {
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { writeText: vi.fn().mockRejectedValue(new Error('denied')) },
    })
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: 0 } },
    })

    await act(async () => {
      root = createRoot(container)
      root.render(
        <QueryClientProvider client={queryClient}>
          <WebRunsPanel
            t={i18n.t.bind(i18n)}
            webRuns={[
              {
                runId: 'run-copy',
                libraryId: 'lib-1',
                seedUrl: 'https://example.test',
                runState: 'completed',
                mode: 'single_page',
                boundaryPolicy: 'same_host',
                maxDepth: 1,
                maxPages: 1,
                crawlFilter: { allowPatterns: [], blockPatterns: [] },
                materializationFilter: { allowPatterns: [], blockPatterns: [] },
              },
            ]}
            onReuseRun={() => {}}
            onCancelRun={() => Promise.resolve()}
          />
        </QueryClientProvider>,
      )
    })

    const copyButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.getAttribute('aria-label') === i18n.t('documents.copyUrl'),
    )
    await act(async () => {
      copyButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    await flushUi()

    expect(toastErrorMock).toHaveBeenCalledWith(i18n.t('documents.urlCopyFailed'))
  })
})
