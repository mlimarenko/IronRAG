import { act, render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { createMemoryRouter, RouterProvider } from 'react-router-dom'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { IngestQueueResponse } from '@/shared/api/generated'

import QueueAdmin from './QueueAdmin'
import i18n from '@/shared/i18n'
import { mapQueue, QueueResponseError, type QueueItem, type QueuePage } from './queue/model'

const queueItem: QueueItem = {
  attemptId: 'attempt-1',
  attemptNumber: 1,
  attemptState: 'running',
  availableAt: '2026-01-01T00:00:00Z',
  canCancel: true,
  canPause: true,
  canResume: false,
  canRetryRequeue: false,
  currentStage: 'embed_chunk',
  documentId: 'document-1',
  documentName: 'Alpha Suite handbook',
  hasStaleQueueLease: false,
  jobId: 'job-1',
  jobKind: 'content_ingest',
  libraryId: 'library-1',
  libraryName: 'Library One',
  progressPercent: 40,
  queuePosition: 1,
  queueState: 'running',
  queuedAt: '2026-01-01T00:00:00Z',
  workspaceId: 'workspace-1',
  workspaceName: 'Workspace One',
}

const mutationState = { error: null as unknown, isPending: false }

type QueuePayload = {
  items?: QueueItem[]
  summary?: { paused: number; queued: number; running: number; total: number }
}

const mocks = vi.hoisted(() => ({
  // The complete payload below renders when this is `null`; a test sets a
  // partial payload to prove the screen survives a response with fields left out.
  queueData: null as QueuePayload | null,
  // TanStack keeps the last successful `data` while a refetch's `select`
  // throws, so a test sets this to model a stale payload plus a live error.
  queueError: null as Error | null,
  requestedScope: null as { libraryId?: string; workspaceId?: string } | null,
  libraryError: null as Error | null,
  libraryPending: false,
  queuePending: false,
  refetchLibrary: vi.fn(),
  refetch: vi.fn(),
  pause: vi.fn(),
  resume: vi.fn(),
  cancel: vi.fn(),
  retry: vi.fn(),
  move: vi.fn(),
  bulk: vi.fn(),
}))

vi.mock('@/shared/contexts/app-context', () => ({
  useApp: () => ({
    activeWorkspace: { id: 'workspace-1', name: 'Workspace One' },
    activeLibrary: { id: 'library-1', name: 'Library One' },
    locale: 'en',
  }),
}))

vi.mock('./queue/api', () => ({
  useQueueLibrary: () => ({
    data: mocks.libraryError ? undefined : { displayName: 'Library Two' },
    error: mocks.libraryError,
    isPending: mocks.libraryPending,
    refetch: mocks.refetchLibrary,
  }),
  useIngestQueue: (scope: { libraryId?: string; workspaceId?: string }) => {
    mocks.requestedScope = scope
    // Exercise the real response boundary: the mock hands `useIngestQueue` a
    // raw payload and applies the same `mapQueue` the query `select` uses, so a
    // malformed payload surfaces as the controlled query error the view handles.
    const payload = (mocks.queueData ?? {
      items: [queueItem],
      summary: { paused: 0, queued: 0, running: 1, total: 1 },
    }) as IngestQueueResponse
    let data: QueuePage | undefined
    let error: unknown = mocks.queueError
    try {
      data = mapQueue(payload)
    } catch (caught) {
      error = error ?? caught
      data = undefined
    }
    return {
      data,
      error,
      isFetching: false,
      isLoading: false,
      isPending: mocks.queuePending,
      refetch: mocks.refetch,
    }
  },
  useQueueMutations: () => ({
    bulk: mocks.bulk,
    bulkState: mutationState,
    cancel: mocks.cancel,
    cancelState: mutationState,
    move: mocks.move,
    moveState: mutationState,
    pause: mocks.pause,
    pauseState: mutationState,
    resume: mocks.resume,
    resumeState: mutationState,
    retry: mocks.retry,
    retryState: mutationState,
  }),
  useQueueInspector: () => ({
    attemptQuery: { data: undefined, isLoading: false },
    attemptsQuery: { data: [], isLoading: false },
    jobQuery: { data: undefined, isLoading: false },
    resolvedAttemptId: null,
    stagesQuery: { data: [], isLoading: false },
  }),
}))

beforeEach(() => {
  mocks.libraryPending = false
  mocks.queuePending = false
  mocks.queueError = null
})

describe('QueueAdmin paused initial requests', () => {
  it('never renders zero counters or an empty queue before the initial queue data arrives', () => {
    mocks.queuePending = true
    renderQueue('/admin/queue?libraryId=target&status=queued')
    expect(screen.queryByText('There are no active ingest jobs.')).toBeNull()
    expect(document.querySelector('.queue-summary')).toBeNull()
    expect(screen.getByText(/Loading/u)).toBeInTheDocument()
  })
  it('waits for a paused explicit library lookup without hiding an invalid-scope error', () => {
    mocks.libraryPending = true
    const { unmount } = renderQueue('/admin/queue?libraryId=target&status=queued')
    expect(document.querySelector('.queue-summary')).toBeNull()
    unmount()
    mocks.queuePending = true
    renderQueue('/admin/queue?libraryId=')
    expect(screen.getByRole('alert')).toBeVisible()
  })
})

function renderQueue(path = '/admin/queue') {
  const router = createMemoryRouter([{ path: '/admin/queue', element: <QueueAdmin /> }], {
    initialEntries: [path],
  })
  return { router, ...render(<RouterProvider router={router} />) }
}

function rowFor(container: HTMLElement, jobId: string): HTMLElement {
  const row = container.querySelector<HTMLElement>(`[data-row-id="${jobId}"]`)
  if (!row) throw new Error(`the queue row for ${jobId} did not render`)
  return row
}

function inspectorIsOpen(): boolean {
  return screen.queryByRole('dialog', { name: queueItem.documentName }) !== null
}

describe('QueueAdmin grid', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    mocks.queueData = null
  })

  it('opens the inspector from the document name', async () => {
    const user = userEvent.setup()
    const { container } = renderQueue()

    await user.click(
      within(rowFor(container, queueItem.jobId)).getByRole('button', {
        name: new RegExp(queueItem.documentName),
      }),
    )

    expect(inspectorIsOpen()).toBe(true)
  })
  it('preserves rapid document filters in the scoped URL before a navigation render', async () => {
    await i18n.changeLanguage('en')
    mocks.queueData = {
      items: [queueItem, { ...queueItem, jobId: 'job-2', documentName: 'Beta handbook' }],
      summary: { paused: 0, queued: 0, running: 2, total: 2 },
    }
    const { router } = renderQueue('/admin/queue?libraryId=target')
    await userEvent
      .setup()
      .click(within(screen.getByRole('columnheader', { name: /Name/i })).getByRole('button'))
    const alpha = screen.getByRole('menuitemcheckbox', { name: queueItem.documentName })
    const beta = screen.getByRole('menuitemcheckbox', { name: 'Beta handbook' })
    act(() => {
      alpha.click()
      beta.click()
    })
    const params = new URLSearchParams(router.state.location.search)
    expect(JSON.parse(params.get('filters')!)).toEqual({
      document_name: [`string:${queueItem.documentName}`, 'string:Beta handbook'],
    })
    expect(params.get('libraryId')).toBe('target')
    expect(alpha).toHaveAttribute('aria-checked', 'true')
    expect(beta).toHaveAttribute('aria-checked', 'true')
    expect(mocks.bulk).not.toHaveBeenCalled()
  })

  it('does not open the inspector from inert row content', async () => {
    // The row toggles selection; the document-name button is the one way into
    // the inspector, so plain cell text must stay inert for keyboard users.
    const user = userEvent.setup()
    const { container } = renderQueue()

    const stageCell = rowFor(container, queueItem.jobId).querySelectorAll('td')[1]
    expect(stageCell).toBeDefined()
    await user.click(stageCell as HTMLElement)

    expect(inspectorIsOpen()).toBe(false)
  })

  it('runs a per-job action without opening the inspector behind it', async () => {
    const user = userEvent.setup()
    const { container } = renderQueue()

    await user.click(
      within(rowFor(container, queueItem.jobId)).getByRole('button', { name: 'Pause job' }),
    )

    expect(mocks.pause).toHaveBeenCalledExactlyOnceWith(queueItem.jobId)
    expect(inspectorIsOpen()).toBe(false)
  })

  it('selects a row from its checkbox without opening the inspector', async () => {
    const user = userEvent.setup()
    const { container } = renderQueue()

    await user.click(within(rowFor(container, queueItem.jobId)).getByRole('checkbox'))

    expect(screen.getByText('1 job selected')).toBeInTheDocument()
    expect(inspectorIsOpen()).toBe(false)
  })

  it('exposes a sort and value-filter menu on every data column', () => {
    renderQueue()

    for (const label of ['Position', 'Name', 'Library', 'Stage', 'Status', 'Progress', 'Attempt']) {
      expect(
        screen.getByRole('button', { name: `Column ${label}: sort and filter` }),
      ).toBeInTheDocument()
    }
  })
})

describe('QueueAdmin malformed response', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    mocks.queueData = null
  })

  it('renders the empty state for a valid empty queue, not an error', () => {
    mocks.queueData = { items: [], summary: { paused: 0, queued: 0, running: 0, total: 0 } }
    renderQueue()

    expect(screen.getByText('Processing queue')).toBeInTheDocument()
    expect(screen.getByText('There are no active ingest jobs.')).toBeInTheDocument()
    expect(screen.queryByRole('alert')).toBeNull()
  })

  it('shows the localized load failure instead of a fake-empty queue when summary is missing', () => {
    mocks.queueData = { items: [] }
    renderQueue()

    expect(screen.getByText('Failed to load ingest queue')).toBeInTheDocument()
    expect(screen.queryByText('There are no active ingest jobs.')).toBeNull()
  })

  it('shows the localized load failure for an empty envelope rather than fake counters', () => {
    mocks.queueData = {}
    renderQueue()

    expect(screen.getByText('Failed to load ingest queue')).toBeInTheDocument()
    expect(document.querySelector('.queue-summary')).toBeNull()
    expect(screen.queryByText('There are no active ingest jobs.')).toBeNull()
  })

  it('keeps the raw field diagnostic out of the user-facing message', () => {
    mocks.queueData = {
      items: [{ ...queueItem, currentStage: { stage: 'embed_chunk' } }],
      summary: { paused: 0, queued: 0, running: 1, total: 1 },
    } as unknown as QueuePayload
    renderQueue()

    expect(screen.getByText('Failed to load ingest queue')).toBeInTheDocument()
    expect(screen.queryByText(/currentStage/)).toBeNull()
  })

  it('localizes the validation failure for the Russian UI', async () => {
    await i18n.changeLanguage('ru')
    mocks.queueData = {}
    renderQueue()

    expect(screen.getByText('Не удалось загрузить очередь обработки')).toBeInTheDocument()
    expect(screen.getByText('Не удалось загрузить экран')).toBeInTheDocument()
    await i18n.changeLanguage('en')
  })

  it('opens the inspector and its controls from a validated response', () => {
    renderQueue('/admin/queue?jobId=job-1')

    expect(screen.getByRole('dialog')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Top' })).toBeInTheDocument()
  })

  it('keeps a stale inspector and its controls unreachable over a refused poll', () => {
    // TanStack keeps the last successful `data` while the refetch's `select`
    // throws, so the view gets a stale payload together with a live error.
    mocks.queueData = {
      items: [queueItem],
      summary: { paused: 0, queued: 0, running: 1, total: 1 },
    }
    mocks.queueError = new QueueResponseError('Ingest queue item 0 queueState must be a string')

    renderQueue('/admin/queue?jobId=job-1')

    expect(screen.getByText('Failed to load ingest queue')).toBeInTheDocument()
    expect(screen.queryByRole('dialog')).toBeNull()
    expect(screen.queryByRole('button', { name: 'Top' })).toBeNull()
    expect(screen.queryByRole('checkbox')).toBeNull()
    expect(screen.queryByText(/job selected/i)).toBeNull()
  })
})

describe('QueueAdmin library drilldown', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    mocks.queueData = null
    mocks.libraryError = null
  })
  it('pins the URL library rather than intersecting it with a different shell workspace', () => {
    mocks.queueData = {
      items: [
        {
          ...queueItem,
          libraryId: 'library-2',
          queueState: 'queued',
          documentName: 'Queued document',
        },
        { ...queueItem, jobId: 'running-2', documentName: 'Running document' },
      ],
      summary: { queued: 1, running: 1, paused: 0, total: 2 },
    }
    renderQueue('/admin/queue?libraryId=library-2&status=queued')
    expect(mocks.requestedScope).toEqual({ libraryId: 'library-2', workspaceId: undefined })
    expect(screen.getByText('Processing queue for Library Two.')).toBeInTheDocument()
    expect(screen.queryByText('Running document')).toBeNull()
    expect(screen.getAllByText('Queued document').length).toBeGreaterThan(0)
  })

  it('preserves library scope and filter changes in a reloadable URL', async () => {
    const { router } = renderQueue('/admin/queue?libraryId=library-2&status=queued')
    await userEvent.setup().click(screen.getByRole('button', { name: /Paused\s*0/ }))
    expect(new URLSearchParams(router.state.location.search).get('libraryId')).toBe('library-2')
    expect(new URLSearchParams(router.state.location.search).get('status')).toBe('paused')
  })

  it('does not fall back to another library for an empty explicit scope', () => {
    renderQueue('/admin/queue?libraryId=')
    expect(mocks.requestedScope).toEqual({ libraryId: '', workspaceId: undefined })
    expect(screen.getByRole('alert')).toBeVisible()
    expect(screen.queryByText(queueItem.documentName)).toBeNull()
  })

  it('explains an unavailable library rather than claiming its queue is empty, and retries both reads', async () => {
    mocks.libraryError = new Error('Library not found')
    mocks.queueData = { items: [], summary: { queued: 0, running: 0, paused: 0, total: 0 } }
    renderQueue('/admin/queue?libraryId=deleted-library&status=queued')
    expect(screen.getByRole('alert')).toBeVisible()
    expect(screen.queryByText('There are no active ingest jobs.')).toBeNull()
    await userEvent.setup().click(screen.getByRole('button', { name: 'Try again' }))
    expect(mocks.refetchLibrary).toHaveBeenCalledTimes(1)
    expect(mocks.refetch).toHaveBeenCalledTimes(1)
  })

  it('keeps the shell-scoped queue usable when its optional title lookup fails', () => {
    mocks.libraryError = new Error('Catalog temporarily unavailable')
    renderQueue()
    expect(screen.queryByRole('alert')).toBeNull()
    expect(screen.getAllByText(queueItem.documentName).length).toBeGreaterThan(0)
    expect(mocks.requestedScope).toEqual({ libraryId: 'library-1', workspaceId: 'workspace-1' })
  })
})

describe('QueueAdmin whole-dataset grid', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    mocks.queueData = null
  })

  it('filters the loaded scoped set by the single table search', async () => {
    const user = userEvent.setup()
    mocks.queueData = {
      items: [
        { ...queueItem, documentName: 'Alpha handbook', jobId: 'job-a' },
        { ...queueItem, documentName: 'Beta runbook', jobId: 'job-b' },
      ],
      summary: { paused: 0, queued: 0, running: 2, total: 2 },
    }
    const { container } = renderQueue()

    await user.type(screen.getByRole('searchbox'), 'beta')

    expect(rowFor(container, 'job-b')).toBeInTheDocument()
    expect(container.querySelector('[data-row-id="job-a"]')).toBeNull()
  })

  it('applies the URL status bucket through the shared grid', () => {
    mocks.queueData = {
      items: [
        { ...queueItem, documentName: 'Queued doc', jobId: 'job-q', queueState: 'queued' },
        { ...queueItem, documentName: 'Paused doc', jobId: 'job-r', queueState: 'paused' },
      ],
      summary: { paused: 1, queued: 1, running: 0, total: 2 },
    }
    const { container } = renderQueue('/admin/queue?status=paused')

    expect(rowFor(container, 'job-r')).toBeInTheDocument()
    expect(container.querySelector('[data-row-id="job-q"]')).toBeNull()
  })

  it('pages past the first window over the complete scoped set', async () => {
    const user = userEvent.setup()
    const items = Array.from({ length: 30 }, (_, index) => ({
      ...queueItem,
      documentName: `Doc ${index}`,
      jobId: `job-${index}`,
      queuePosition: index + 1,
    }))
    mocks.queueData = { items, summary: { paused: 0, queued: 0, running: 30, total: 30 } }
    const { container } = renderQueue('/admin/queue?pageSize=25')

    expect(rowFor(container, 'job-24')).toBeInTheDocument()
    expect(container.querySelector('[data-row-id="job-25"]')).toBeNull()

    await user.click(screen.getByRole('button', { name: 'Next' }))

    expect(rowFor(container, 'job-25')).toBeInTheDocument()
    expect(container.querySelector('[data-row-id="job-24"]')).toBeNull()
  })

  it('reloads a stage value filter by its raw key while sorting by the label', () => {
    mocks.queueData = {
      items: [
        { ...queueItem, currentStage: 'embed_chunk', jobId: 'job-embed' },
        { ...queueItem, currentStage: 'extract_graph', jobId: 'job-graph' },
      ],
      summary: { paused: 0, queued: 0, running: 2, total: 2 },
    }
    // The raw stage token is the URL/filter identity: a reload with the raw key
    // still selects the row, and the row is found by its raw token.
    renderQueue(
      `/admin/queue?filters=${encodeURIComponent('{"current_stage":["string:embed_chunk"]}')}`,
    )
    expect(screen.getByText('Embedding chunks')).toBeInTheDocument()
    expect(screen.queryByText('Extract graph')).toBeNull()
  })

  it('orders the stage column by the displayed label, not the raw token', async () => {
    await i18n.changeLanguage('ru')
    const user = userEvent.setup()
    mocks.queueData = {
      items: [
        { ...queueItem, currentStage: 'chunk_content', jobId: 'job-chunk' },
        { ...queueItem, currentStage: 'embed_chunk', jobId: 'job-embed' },
        { ...queueItem, currentStage: null, jobId: 'job-null' },
      ],
      summary: { paused: 0, queued: 0, running: 3, total: 3 },
    }
    const { container } = renderQueue()
    const stageHeader = screen.getByRole('columnheader', { name: /Этап/i })
    await user.click(within(stageHeader).getByRole('button'))
    await user.click(await screen.findByRole('menuitem', { name: /возрастанию/i }))
    // RU labels: 'Векторизация…' (embed_chunk) < 'Нарезка…' (chunk_content),
    // which is the reverse of the raw token order, so a raw sort would fail.
    const rows = [...container.querySelectorAll('[data-row-id]')].map((row) =>
      row.getAttribute('data-row-id'),
    )
    expect(rows).toEqual(['job-embed', 'job-chunk', 'job-null'])
    await user.click(within(stageHeader).getByRole('button'))
    await user.click(await screen.findByRole('menuitem', { name: /убыванию/i }))
    expect(
      [...container.querySelectorAll('[data-row-id]')].map((row) =>
        row.getAttribute('data-row-id'),
      ),
    ).toEqual(['job-chunk', 'job-embed', 'job-null'])
    await i18n.changeLanguage('en')
  })
})
