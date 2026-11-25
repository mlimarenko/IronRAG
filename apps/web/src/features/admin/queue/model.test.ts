import { fireEvent, render } from '@testing-library/react'
import { createElement, type ReactElement } from 'react'
import { describe, expect, it, vi } from 'vitest'

import type {
  IngestAttempt,
  IngestAttemptResponse,
  IngestJob,
  IngestJobResponse,
  IngestQueueResponse,
  IngestStageEvent,
  IngestStageTimelineResponse,
} from '@/shared/api/generated'
import i18n from '@/shared/i18n'

import {
  mapAttempt,
  mapAttempts,
  mapJob,
  mapQueue,
  mapStageEvents,
  parseQueueFilters,
  parseQueueSort,
  queueBucketFor,
  queueBucketKeys,
  queueFiltersFromUrl,
  queueGridColumns,
  QueueResponseError,
  queueParamsWithState,
  queueScreenPhase,
  resolveQueueRoute,
  serializeQueueFilters,
  type QueueItem,
} from './model'

const validQueueItem = {
  availableAt: '2026-08-09T10:00:00Z',
  canCancel: true,
  canPause: false,
  canResume: false,
  canRetryRequeue: true,
  documentName: 'handbook.pdf',
  hasStaleQueueLease: true,
  jobId: 'job-1',
  jobKind: 'document_ingest',
  libraryId: 'library-1',
  libraryName: 'Handbook',
  queueState: 'leased',
  queuedAt: '2026-08-09T09:00:00Z',
  workspaceId: 'workspace-1',
  workspaceName: 'Platform',
}

const validQueueSummary = { paused: 0, queued: 0, running: 1, total: 1 }

describe('mapQueue', () => {
  it('preserves server capabilities and summary values', () => {
    const response: IngestQueueResponse = {
      items: [validQueueItem],
      summary: { paused: 0, queued: 0, running: 1, total: 1 },
    }

    const result = mapQueue(response)

    expect(result.summary).toEqual({ paused: 0, queued: 0, running: 1, total: 1 })
    expect(result.items[0]).toMatchObject({
      canCancel: true,
      canRetryRequeue: true,
      hasStaleQueueLease: true,
    })
  })

  it('accepts a valid empty queue without fabricating rows or counters', () => {
    const response: IngestQueueResponse = {
      items: [],
      summary: { paused: 0, queued: 0, running: 0, total: 0 },
    }

    expect(mapQueue(response)).toEqual(response)
  })

  it('accepts every declared nullable field as null, absent or its own type', () => {
    const withNulls = {
      ...validQueueItem,
      attemptId: null,
      attemptNumber: null,
      attemptState: null,
      currentStage: null,
      documentId: null,
      failureCode: null,
      failureMessage: null,
      heartbeatAt: null,
      progressPercent: null,
      queuePosition: null,
      startedAt: null,
    }
    const withValues = {
      ...validQueueItem,
      attemptId: 'attempt-1',
      attemptNumber: 2,
      attemptState: 'running',
      currentStage: 'embed_chunk',
      documentId: 'document-1',
      failureCode: 'stale_heartbeat',
      failureMessage: 'lease expired',
      heartbeatAt: '2026-08-09T10:00:00Z',
      progressPercent: 55,
      queuePosition: 3,
      startedAt: '2026-08-09T09:30:00Z',
    }

    // The optional keys are omitted entirely on `validQueueItem`.
    expect(() => mapQueue({ items: [validQueueItem], summary: validQueueSummary })).not.toThrow()
    expect(mapQueue({ items: [withNulls], summary: validQueueSummary }).items[0]).toEqual(withNulls)
    expect(mapQueue({ items: [withValues], summary: validQueueSummary }).items[0]).toEqual(
      withValues,
    )
  })

  it.each([
    ['a non-object response', 42],
    ['a missing envelope', {}],
    ['a null items field', { items: null, summary: validQueueSummary }],
    ['a non-array items field', { items: 'not-an-array', summary: validQueueSummary }],
    ['a missing summary', { items: [] }],
    ['a null summary', { items: [], summary: null }],
    ['a non-object summary', { items: [], summary: 'counts' }],
  ])('rejects %s with a controlled queue error', (_label, payload) => {
    expect(() => mapQueue(payload as unknown as IngestQueueResponse)).toThrow(QueueResponseError)
  })

  it.each([
    [
      'items field is not an array',
      { items: 'not-an-array', summary: validQueueSummary },
      'Ingest queue response items must be an array',
    ],
    ['summary is missing', { items: [] }, 'Ingest queue response summary must be an object'],
  ])('names the envelope violation when %s', (_label, payload, message) => {
    expect(() => mapQueue(payload as unknown as IngestQueueResponse)).toThrow(message)
  })

  it.each([
    ['queueState', null],
    ['queueState', undefined],
    ['jobKind', 7],
    ['documentName', { text: 'handbook.pdf' }],
  ])('rejects an item whose required %s is not a string', (field, value) => {
    const response = {
      items: [{ ...validQueueItem, [field]: value }],
      summary: validQueueSummary,
    } as unknown as IngestQueueResponse

    expect(() => mapQueue(response)).toThrow(`Ingest queue item 0 ${field} must be a string`)
  })

  it.each([
    ['canPause', 1],
    ['canResume', null],
    ['canCancel', 'true'],
    ['canRetryRequeue', undefined],
    ['hasStaleQueueLease', 0],
  ])('rejects an item whose required %s is not a boolean', (field, value) => {
    const response = {
      items: [{ ...validQueueItem, [field]: value }],
      summary: validQueueSummary,
    } as unknown as IngestQueueResponse

    expect(() => mapQueue(response)).toThrow(`Ingest queue item 0 ${field} must be a boolean`)
  })

  it.each([
    ['currentStage', { stage: 'embed_chunk' }],
    ['currentStage', 7],
    ['failureCode', 42],
    ['failureMessage', { text: 'boom' }],
    ['attemptState', false],
    ['documentId', 1],
    ['heartbeatAt', []],
    ['startedAt', 0],
  ])('rejects a non-string non-null %s that would reach a humanizer', (field, value) => {
    const response = {
      items: [{ ...validQueueItem, [field]: value }],
      summary: validQueueSummary,
    } as unknown as IngestQueueResponse

    expect(() => mapQueue(response)).toThrow(
      `Ingest queue item 0 ${field} must be a string or null`,
    )
  })

  it.each([
    ['attemptNumber', 1.5],
    ['progressPercent', 0.5],
    ['queuePosition', 1.5],
    ['progressPercent', '55'],
    ['progressPercent', Number.NaN],
    ['queuePosition', { rank: 1 }],
    ['attemptNumber', '2'],
    ['queuePosition', Number.POSITIVE_INFINITY],
  ])('rejects a non-integer %s that would become a NaN or a fraction', (field, value) => {
    const response = {
      items: [{ ...validQueueItem, [field]: value }],
      summary: validQueueSummary,
    } as unknown as IngestQueueResponse

    expect(() => mapQueue(response)).toThrow(
      `Ingest queue item 0 ${field} must be an integer or null`,
    )
  })

  it.each([
    ['paused', 0.5],
    ['queued', 1.5],
    ['running', 2.5],
    ['total', 3.5],
    ['paused', '0'],
    ['queued', Number.NaN],
    ['running', null],
    ['total', { count: 1 }],
  ])('rejects a summary whose %s counter is not an integer', (field, value) => {
    const response = {
      items: [],
      summary: { paused: 0, queued: 0, running: 0, total: 0, [field]: value },
    } as unknown as IngestQueueResponse

    expect(() => mapQueue(response)).toThrow(
      `Ingest queue response summary ${field} must be an integer`,
    )
  })

  it.each([[null], ['item'], [42]])(
    'rejects an item that is not an object with its position: %s',
    (item) => {
      const response = {
        items: [item],
        summary: validQueueSummary,
      } as unknown as IngestQueueResponse

      expect(() => mapQueue(response)).toThrow('Ingest queue item 0 must be an object')
    },
  )

  it('rejects a response that repeats a jobId before the renderer keys rows by it', () => {
    const response: IngestQueueResponse = {
      items: [validQueueItem, { ...validQueueItem, documentName: 'other.pdf' }],
      summary: validQueueSummary,
    }

    expect(() => mapQueue(response)).toThrow(
      new QueueResponseError('Ingest queue response repeats jobId job-1'),
    )
  })
})

describe('resolveQueueRoute', () => {
  it('keeps implicit scope in the selected shell library and workspace', () => {
    expect(resolveQueueRoute(new URLSearchParams(), 'shell-lib', 'shell-ws')).toEqual({
      explicitLibraryId: null,
      scope: { libraryId: 'shell-lib', workspaceId: 'shell-ws' },
      stateFilter: 'all',
    })
  })
  it('pins an explicit library independently of shell workspace', () => {
    expect(
      resolveQueueRoute(
        new URLSearchParams('libraryId=target&status=queued'),
        'shell-lib',
        'shell-ws',
      ),
    ).toEqual({
      explicitLibraryId: 'target',
      scope: { libraryId: 'target', workspaceId: undefined },
      stateFilter: 'queued',
    })
  })
  it.each(['libraryId=', 'libraryId=+++'])(
    'keeps an empty explicit scope fail-closed: %s',
    (query) => {
      const route = resolveQueueRoute(new URLSearchParams(query), 'shell-lib', 'shell-ws')
      expect(route.explicitLibraryId).toBe('')
      expect(route.scope).toEqual({ libraryId: '', workspaceId: undefined })
    },
  )
  it.each(['all', 'failed', 'paused', 'queued', 'running'])(
    'preserves the %s state filter',
    (status) => {
      expect(
        resolveQueueRoute(new URLSearchParams({ status }), undefined, undefined).stateFilter,
      ).toBe(status)
    },
  )
  it('does not mistake an unknown state for another specific bucket', () => {
    expect(
      resolveQueueRoute(new URLSearchParams('status=unknown'), undefined, undefined).stateFilter,
    ).toBe('all')
  })
})

describe('queueParamsWithState', () => {
  it.each(['all', 'failed', 'paused', 'queued', 'running'] as const)(
    'changes only the %s filter without mutating the input',
    (state) => {
      const current = new URLSearchParams('libraryId=target&status=queued&q=needle')
      const next = queueParamsWithState(current, state)
      expect(next.get('libraryId')).toBe('target')
      expect(next.get('q')).toBe('needle')
      expect(next.get('status')).toBe(state === 'all' ? null : state)
      expect(current.get('status')).toBe('queued')
    },
  )
})

describe('queueScreenPhase', () => {
  const ready = { isPending: false, isLoading: false, error: null }
  const paused = { isPending: true, isLoading: false, error: null }
  it('does not claim a paused initial queue request is an empty loaded queue', () => {
    expect(queueScreenPhase('target', paused, ready)).toBe('loading')
  })
  it('waits for a paused explicit library lookup even when the queue is loaded', () => {
    expect(queueScreenPhase('target', ready, paused)).toBe('loading')
  })
  it('keeps an invalid explicit scope in error even though its disabled queries are pending', () => {
    expect(queueScreenPhase('', paused, paused)).toBe('error')
  })
  it('fails closed for an empty or unavailable explicit library', () => {
    expect(queueScreenPhase('', ready, ready)).toBe('error')
    expect(queueScreenPhase('missing', ready, { ...ready, error: new Error('not found') })).toBe(
      'error',
    )
  })
  it('waits for the explicit library lookup and queue data', () => {
    expect(queueScreenPhase('target', ready, { ...ready, isPending: true })).toBe('loading')
    expect(queueScreenPhase(null, { ...ready, isPending: true }, ready)).toBe('loading')
  })
  it('does not hide shell-scoped rows behind a failed optional title lookup', () => {
    expect(queueScreenPhase(null, ready, { isPending: false, error: new Error('title') })).toBe(
      'ready',
    )
  })
})

describe('queue buckets', () => {
  it('maps a named bucket to the queue states it covers', () => {
    expect(queueBucketKeys('running').toSorted()).toEqual(['string:leased', 'string:running'])
    expect(queueBucketKeys('paused')).toEqual(['string:paused'])
    expect(queueBucketKeys('all')).toEqual([])
  })

  it('reads a selection back as the bucket it equals, and all otherwise', () => {
    expect(queueBucketFor(queueBucketKeys('running'))).toBe('running')
    expect(queueBucketFor(['string:leased'])).toBe('all')
    expect(queueBucketFor(['string:queued', 'string:paused'])).toBe('all')
    expect(queueBucketFor(undefined)).toBe('all')
  })

  it('seeds the queue_state filter from the legacy status alias', () => {
    expect(queueFiltersFromUrl('queued', null)).toEqual({ queue_state: ['string:queued'] })
    expect(queueFiltersFromUrl('all', null)).toEqual({})
    // An explicit filters entry wins over the alias.
    expect(
      queueFiltersFromUrl('queued', JSON.stringify({ queue_state: ['string:paused'] })),
    ).toEqual({ queue_state: ['string:paused'] })
  })
})

describe('queue grid URL encoding', () => {
  it('round-trips filters and drops unknown columns and malformed keys', () => {
    const filters = { library_name: ['string:Manuals'], queue_state: ['null'] }
    const encoded = serializeQueueFilters(filters)
    expect(encoded).not.toBeNull()
    expect(parseQueueFilters(encoded)).toEqual(filters)
    expect(serializeQueueFilters({ library_name: [] })).toBeNull()
    expect(parseQueueFilters('not json')).toEqual({})
    expect(
      parseQueueFilters(JSON.stringify({ nope: ['string:x'], library_name: ['garbage'] })),
    ).toEqual({})
  })

  it('parses only known queue columns and directions', () => {
    expect(parseQueueSort('queue_position:asc')).toEqual({
      columnId: 'queue_position',
      direction: 'asc',
    })
    expect(parseQueueSort('queue_state:desc')).toEqual({
      columnId: 'queue_state',
      direction: 'desc',
    })
    expect(parseQueueSort('unknown:asc')).toBeNull()
    expect(parseQueueSort('queue_position:sideways')).toBeNull()
    expect(parseQueueSort(null)).toBeNull()
  })

  it('drops a filters document that is not a JSON object', () => {
    for (const raw of ['[]', '42', 'null', '"text"', '{"queue_state":[]}']) {
      expect(parseQueueFilters(raw)).toEqual({})
    }
  })
})

describe('queue row inspection mapping', () => {
  const job: IngestJob = {
    available_at: '2026-08-09T10:00:00Z',
    id: 'job-1',
    job_kind: 'document_ingest',
    library_id: 'library-1',
    priority: 0,
    queue_state: 'running',
    queued_at: '2026-08-09T09:00:00Z',
    workspace_id: 'workspace-1',
  }
  const readiness = { graphReady: false, textReady: false, vectorReady: false }
  const attempt: IngestAttempt = {
    attempt_number: 1,
    attempt_state: 'running',
    id: 'attempt-1',
    job_id: 'job-1',
    progress_percent: 40,
    retryable: true,
    started_at: '2026-08-09T09:30:00Z',
  }

  it('passes the queue inspector payloads through without rewriting them', () => {
    const jobResponse: IngestJobResponse = { job, readiness }
    const attemptResponse: IngestAttemptResponse = { attempt, job, readiness }
    const attempts = { items: [attempt] }

    expect(mapJob(jobResponse)).toBe(jobResponse)
    expect(mapAttempt(attemptResponse)).toBe(attemptResponse)
    expect(mapAttempts(attempts)).toBe(attempts.items)
  })

  it('orders the stage timeline by its own ordinal, not by arrival', () => {
    const stage = (id: string, ordinal: number): IngestStageEvent => ({
      attempt_id: 'attempt-1',
      details_json: null,
      id,
      ordinal,
      recorded_at: '2026-08-09T09:30:00Z',
      stage_name: id,
      stage_state: 'completed',
    })
    const timeline: IngestStageTimelineResponse = {
      attempt,
      job,
      readiness,
      stages: [stage('index', 3), stage('extract', 1), stage('embed', 2)],
    }

    expect(mapStageEvents(timeline).map((event) => event.stage_name)).toEqual([
      'extract',
      'embed',
      'index',
    ])
  })
})

describe('queue grid columns', () => {
  const row = (overrides: Partial<QueueItem> = {}): QueueItem => ({
    ...validQueueItem,
    ...overrides,
  })
  const columns = (onOpen = () => {}) => queueGridColumns({ onOpen, t: i18n.t })
  const column = (id: string) => columns().find((candidate) => candidate.id === id)!
  const markup = (id: string, item: QueueItem) =>
    render(column(id).cell!(item) as ReactElement).container.textContent

  it('shows a dash and an empty display text for a row without a queue position or attempt', () => {
    expect(column('queue_position').value(row())).toBeNull()
    expect(column('queue_position').displayText?.(row())).toBe('')
    expect(markup('queue_position', row())).toBe('—')
    expect(column('queue_position').value(row({ queuePosition: 3 }))).toBe(3)
    expect(column('queue_position').displayText?.(row({ queuePosition: 3 }))).toBe('3')
    expect(markup('queue_position', row({ queuePosition: 3 }))).toBe('3')

    expect(column('attempt_number').value(row())).toBeNull()
    expect(column('attempt_number').displayText?.(row())).toBe('')
    expect(markup('attempt_number', row())).toBe('—')
    expect(column('attempt_number').value(row({ attemptNumber: 2 }))).toBe(2)
    expect(column('attempt_number').displayText?.(row({ attemptNumber: 2 }))).toBe('2')
  })

  it('opens the inspector from the document name without also selecting the row', () => {
    const onOpen = vi.fn()
    const parentClick = vi.fn()
    const item = row()
    const documentColumn = queueGridColumns({ onOpen, t: i18n.t }).find(
      (candidate) => candidate.id === 'document_name',
    )!
    const { container } = render(
      createElement('div', { onClick: parentClick }, documentColumn.cell!(item) as ReactElement),
    )

    fireEvent.click(container.querySelector('button')!)

    expect(onOpen).toHaveBeenCalledWith(item)
    expect(parentClick).not.toHaveBeenCalled()
    expect(container.textContent).toContain('handbook.pdf')
    expect(container.textContent).toContain('Document ingest')
    expect(documentColumn.value(item)).toBe('handbook.pdf')
    expect(documentColumn.searchText?.(item)).toBe('handbook.pdf Document ingest')
  })

  it('shows the library and its workspace together', () => {
    const item = row()
    expect(column('library_name').value(item)).toBe('Handbook')
    expect(markup('library_name', item)).toBe('HandbookPlatform')
    expect(column('library_name').searchText?.(item)).toBe('Handbook Platform')
  })

  it('sorts and labels the stage by its humanized label', () => {
    expect(column('current_stage').value(row())).toBeNull()
    expect(column('current_stage').displayText?.(row())).toBe('')
    expect(column('current_stage').sortValue?.(row())).toBe('')
    expect(markup('current_stage', row())).toBe('—')

    const staged = row({ currentStage: 'embed_chunk' })
    expect(column('current_stage').value(staged)).toBe('embed_chunk')
    expect(column('current_stage').displayText?.(staged)).toBe('Embedding chunks')
    expect(column('current_stage').sortValue?.(staged)).toBe('Embedding chunks')
    expect(markup('current_stage', staged)).toBe('Embedding chunks')
  })

  it('shows the queue state label, the stale-lease badge and the failure reason', () => {
    expect(column('queue_state').value(row())).toBe('leased')
    expect(column('queue_state').displayText?.(row())).toBe('Running')
    expect(markup('queue_state', row())).toBe('RunningStale lease')
    expect(markup('queue_state', row({ hasStaleQueueLease: false }))).toBe('Running')

    // A stale heartbeat is already explained by the stale-lease badge.
    expect(markup('queue_state', row({ failureCode: 'stale_heartbeat' }))).toBe(
      'RunningStale lease',
    )

    const failed = row({
      failureCode: 'ocr_failed',
      hasStaleQueueLease: false,
      queueState: 'failed',
    })
    expect(markup('queue_state', failed)).toBe('FailedProcessing failed: Ocr failed.')
    expect(column('queue_state').searchText?.(failed)).toBe(
      'Failed failed Processing failed: Ocr failed.',
    )
    // A row without a failure reason is searchable by its label and raw state only.
    expect(column('queue_state').searchText?.(row({ hasStaleQueueLease: false }))).toBe(
      'Running leased',
    )
  })

  it('renders the progress bar only for a known percentage', () => {
    expect(column('progress_percent').value(row())).toBeNull()
    expect(column('progress_percent').displayText?.(row())).toBe('')
    expect(markup('progress_percent', row())).toBe('—')

    const running = row({ progressPercent: 55 })
    expect(column('progress_percent').value(running)).toBe(55)
    expect(column('progress_percent').displayText?.(running)).toBe('55%')
    const { container } = render(column('progress_percent').cell!(running) as ReactElement)
    expect(container.textContent).toBe('55%')
    expect(container.querySelector('progress')?.getAttribute('aria-valuenow')).toBe('55')
    expect(container.querySelector('progress')?.getAttribute('aria-label')).toBe('Progress')
  })
})
