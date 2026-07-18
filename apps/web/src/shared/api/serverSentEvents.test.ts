import { describe, expect, it, vi } from 'vitest'

import { createSseClient } from './generated/core/serverSentEvents.gen'

function controlledSseResponse(): {
  controller: ReadableStreamDefaultController<Uint8Array>
  response: Response
} {
  let controller: ReadableStreamDefaultController<Uint8Array> | undefined
  const body = new ReadableStream<Uint8Array>({
    start(nextController) {
      controller = nextController
    },
  })
  if (!controller) {
    throw new Error('SSE test stream controller was not initialized')
  }
  return { controller, response: new Response(body, { status: 200 }) }
}

function encodeEvent(data: string, fields = ''): Uint8Array {
  return new TextEncoder().encode(`${fields}data: ${data}\n\n`)
}

async function collect<T>(stream: AsyncGenerator<T>): Promise<T[]> {
  const values: T[] = []
  for await (const value of stream) {
    values.push(value)
  }
  return values
}

describe('generated SSE client', () => {
  it('yields complete events before the response stream closes', async () => {
    const { controller, response } = controlledSseResponse()
    const client = createSseClient<string>({
      fetch: vi.fn().mockResolvedValue(response),
      url: 'https://example.invalid/events',
    })

    const firstValue = client.stream.next()
    controller.enqueue(encodeEvent('first'))
    const observed = await Promise.race([
      firstValue,
      new Promise<'pending'>((resolve) => setTimeout(() => resolve('pending'), 50)),
    ])
    controller.close()
    await firstValue

    expect(observed).toEqual({ done: false, value: 'first' })
  })

  it('validates and transforms JSON before notifying and yielding', async () => {
    const { controller, response } = controlledSseResponse()
    const responseValidator = vi.fn().mockResolvedValue(undefined)
    const responseTransformer = vi.fn().mockResolvedValue({ value: 'transformed' })
    const onSseEvent = vi.fn()
    const client = createSseClient<{ value: string }>({
      fetch: vi.fn().mockResolvedValue(response),
      onSseEvent,
      responseTransformer,
      responseValidator,
      url: 'https://example.invalid/events',
    })

    controller.enqueue(encodeEvent('{"value":"original"}', 'event: update\nid: event-1\n'))
    controller.close()

    await expect(collect(client.stream)).resolves.toEqual([{ value: 'transformed' }])
    expect(responseValidator).toHaveBeenCalledWith({ value: 'original' })
    expect(onSseEvent).toHaveBeenCalledWith({
      data: { value: 'transformed' },
      event: 'update',
      id: 'event-1',
      retry: 3000,
    })
  })

  it('sends the latest event ID when reconnecting after a stream error', async () => {
    let firstPull = true
    const firstBody = new ReadableStream<Uint8Array>({
      pull(controller) {
        if (firstPull) {
          firstPull = false
          controller.enqueue(encodeEvent('first', 'id: event-1\n'))
          return
        }
        throw new Error('connection interrupted')
      },
    })
    const secondBody = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(encodeEvent('second'))
        controller.close()
      },
    })
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(new Response(firstBody, { status: 200 }))
      .mockResolvedValueOnce(new Response(secondBody, { status: 200 }))
    const client = createSseClient<string>({
      fetch,
      sseMaxRetryAttempts: 2,
      sseSleepFn: vi.fn().mockResolvedValue(undefined),
      url: 'https://example.invalid/events',
    })

    await expect(collect(client.stream)).resolves.toEqual(['first', 'second'])
    const secondRequest = fetch.mock.calls[1]?.[0]

    expect(secondRequest).toBeInstanceOf(Request)
    expect((secondRequest as Request).headers.get('Last-Event-ID')).toBe('event-1')
  })
})
