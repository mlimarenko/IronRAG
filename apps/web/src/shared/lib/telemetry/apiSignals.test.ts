import { afterEach, describe, expect, it, vi } from 'vitest'

import { createProductSignalsSpanProcessor } from './apiSignals'
import { setProductEventSink } from './events'

type Processor = ReturnType<typeof createProductSignalsSpanProcessor>
type Span = Parameters<Processor['onEnd']>[0]

type RecordedEvent = { name: string; attributes: Record<string, string | number | boolean> }
type RecordedFailure = { value: number; attributes: Record<string, string | number | boolean> }

function harness() {
  const events: RecordedEvent[] = []
  const failures: RecordedFailure[] = []
  const meter = {
    createCounter: () => ({
      add: (value: number, attributes?: Record<string, string | number | boolean>) => {
        failures.push({ value, attributes: attributes ?? {} })
      },
    }),
  }
  setProductEventSink((name, attributes) => {
    events.push({ name, attributes })
  })
  return { processor: createProductSignalsSpanProcessor({ meter }), events, failures }
}

function span(
  attributes: Record<string, unknown>,
  duration: [number, number] = [0, 0],
  kind = 2,
): Span {
  return { kind, attributes, duration } as unknown as Span
}

afterEach(() => {
  setProductEventSink(null)
  vi.unstubAllGlobals()
})

describe('product API signals span processor', () => {
  it('ignores spans that are not client HTTP calls', () => {
    const { processor, events, failures } = harness()

    processor.onEnd(
      span({ 'http.request.method': 'POST', 'url.full': '/v1/iam/session' }, [0, 0], 1),
    )

    expect(failures).toEqual([])
    expect(events).toEqual([])
  })

  it('ignores client spans without a method or outside the product API', () => {
    const { processor, events, failures } = harness()

    processor.onEnd(span({ 'url.full': '/v1/iam/session' }))
    processor.onEnd(span({ 'http.request.method': 42, 'url.full': '/v1/iam/session' }))
    processor.onEnd(span({ 'http.request.method': 'GET', 'url.full': '/health' }))
    processor.onEnd(span({ 'http.request.method': 'GET' }))

    expect(failures).toEqual([])
    expect(events).toEqual([])
  })

  it('counts a transport failure on an unknown route without inventing a product event', () => {
    const { processor, events, failures } = harness()

    processor.onEnd(span({ 'http.request.method': 'GET', 'url.full': '/v1/unknown' }))

    expect(events).toEqual([])
    expect(failures).toEqual([
      {
        value: 1,
        attributes: expect.objectContaining({
          'http.request.method': 'GET',
          'http.route': '/v1/unknown',
          'http.response.status_code': 'none',
          'error.type': 'network',
        }),
      },
    ])
  })

  it('classifies a 4xx login failure and emits the failure event', () => {
    const { processor, events, failures } = harness()

    processor.onEnd(
      span({
        'http.request.method': 'POST',
        'url.full': 'http://localhost:19000/v1/iam/session?next=/documents',
        'http.response.status_code': 401,
      }),
    )

    expect(failures).toEqual([
      {
        value: 1,
        attributes: expect.objectContaining({
          'http.route': '/v1/iam/session',
          'http.response.status_code': 401,
          'error.type': 'http_4xx',
        }),
      },
    ])
    expect(events).toEqual([
      {
        name: 'login_failed',
        attributes: expect.objectContaining({
          feature: 'api',
          'http.route': '/v1/iam/session',
          'http.response.status_code': 401,
          'error.type': 'http_error',
        }),
      },
    ])
  })

  it('classifies a 5xx query failure and keeps the route pattern, not the identifiers', () => {
    const { processor, events, failures } = harness()

    processor.onEnd(
      span({
        'http.request.method': 'POST',
        'url.full': '/v1/query/sessions/2d9e00d8-1c42-4bdc-8f35-0f8f46c2a3d4/turns',
        'http.response.status_code': 503,
      }),
    )

    expect(failures[0]?.attributes['error.type']).toBe('http_5xx')
    expect(failures[0]?.attributes['http.route']).toBe('/v1/query/sessions/:id/turns')
    expect(events).toEqual([
      {
        name: 'query_failed',
        attributes: expect.objectContaining({
          'http.route': '/v1/query/sessions/:sessionId/turns',
          'error.type': 'http_error',
        }),
      },
    ])
  })

  it.each([
    ['/v1/iam/session', 'POST', 'login_succeeded'],
    [
      '/v1/query/libraries/0192f6a4-6d1f-7b03-9a02-3f6b6f1a2b17/sessions',
      'POST',
      'query_session_started',
    ],
    ['/v1/query/sessions/0192f6a4-6d1f-7b03-9a02-3f6b6f1a2b17/turns', 'POST', 'query_completed'],
    [
      '/v1/content/libraries/0192f6a4-6d1f-7b03-9a02-3f6b6f1a2b17/documents',
      'POST',
      'document_uploaded',
    ],
    [
      '/v1/catalog/workspaces/0192f6a4-6d1f-7b03-9a02-3f6b6f1a2b17/libraries',
      'POST',
      'library_created',
    ],
    ['/v1/ingest/libraries/0192f6a4-6d1f-7b03-9a02-3f6b6f1a2b17/jobs', 'POST', 'ingest_started'],
  ])('emits the success event for %s', (url, method, eventName) => {
    const { processor, events, failures } = harness()

    processor.onEnd(
      span({ 'http.request.method': method, 'url.full': url, 'http.response.status_code': 200 }),
    )

    expect(failures).toEqual([])
    expect(events).toEqual([
      {
        name: eventName,
        attributes: expect.objectContaining({ feature: 'api', 'error.type': 'none' }),
      },
    ])
  })

  it('does not emit a product event for a successful route outside the contract', () => {
    const { processor, events, failures } = harness()

    processor.onEnd(
      span({
        'http.request.method': 'GET',
        'url.full': '/v1/iam/session',
        'http.response.status_code': 200,
      }),
    )

    expect(failures).toEqual([])
    expect(events).toEqual([])
  })

  it('accepts a numeric status string and rejects a non-numeric one as a network failure', () => {
    const { processor, events, failures } = harness()

    processor.onEnd(
      span({
        'http.request.method': 'POST',
        'url.full': '/v1/iam/session',
        'http.response.status_code': '200',
      }),
    )
    expect(events.map((event) => event.name)).toEqual(['login_succeeded'])

    processor.onEnd(
      span({
        'http.request.method': 'POST',
        'url.full': '/v1/iam/session',
        'http.response.status_code': 'not-a-status',
      }),
    )
    expect(failures[0]?.attributes['error.type']).toBe('network')
    expect(events.at(-1)).toMatchObject({
      name: 'login_failed',
      attributes: expect.objectContaining({ 'error.type': 'network' }),
    })
  })

  it('rounds the span duration into milliseconds', () => {
    const { processor, events } = harness()

    processor.onEnd(
      span(
        {
          'http.request.method': 'POST',
          'url.full': '/v1/iam/session',
          'http.response.status_code': 302,
        },
        [1, 500_000_000],
      ),
    )

    expect(events[0]?.attributes.duration_ms).toBe(1500)
  })

  it('treats 400 as a failure and 399 as success at the boundary', () => {
    const { processor, events, failures } = harness()

    processor.onEnd(
      span({
        'http.request.method': 'POST',
        'url.full': '/v1/ingest/libraries/0192f6a4-6d1f-7b03-9a02-3f6b6f1a2b17/jobs',
        'http.response.status_code': 400,
      }),
    )
    expect(failures[0]?.attributes['error.type']).toBe('http_4xx')

    processor.onEnd(
      span({
        'http.request.method': 'POST',
        'url.full': '/v1/ingest/libraries/0192f6a4-6d1f-7b03-9a02-3f6b6f1a2b17/jobs',
        'http.response.status_code': 399,
      }),
    )
    expect(events.at(-1)?.name).toBe('ingest_started')
  })

  it('falls back to the raw URL when it cannot be parsed', () => {
    const { processor, events, failures } = harness()

    processor.onEnd(span({ 'http.request.method': 'GET', 'url.full': 'http://[::1' }))

    expect(events).toEqual([])
    expect(failures).toEqual([])
  })

  it('parses a relative URL without a browser location', () => {
    const { processor, events } = harness()

    vi.stubGlobal('window', undefined)
    processor.onEnd(
      span({
        'http.request.method': 'POST',
        'url.full': '/v1/iam/session',
        'http.response.status_code': 200,
      }),
    )

    expect(events.map((event) => event.name)).toEqual(['login_succeeded'])
  })

  it('resolves its shutdown and flush hooks', async () => {
    const { processor } = harness()

    processor.onStart({} as never, {} as never)
    await expect(processor.shutdown()).resolves.toBeUndefined()
    await expect(processor.forceFlush()).resolves.toBeUndefined()
  })
})
