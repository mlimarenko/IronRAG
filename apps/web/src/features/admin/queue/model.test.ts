import { describe, expect, it } from 'vitest'

import type { IngestQueueResponse } from '@/shared/api/generated'

import {
  mapQueue,
  parseQueueFilters,
  parseQueueSort,
  queueBucketFor,
  queueBucketKeys,
  queueFiltersFromUrl,
  QueueResponseError,
  queueParamsWithState,
  queueScreenPhase,
  resolveQueueRoute,
  serializeQueueFilters,
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
})
