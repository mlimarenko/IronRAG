import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { client } from './generated/client.gen'
import {
  installSessionExpiryInterceptor,
  markSessionSignedIn,
  markSessionSignedOut,
  reportUnauthorizedResponse,
  subscribeToSessionExpired,
} from './sessionLifecycle'

describe('session lifecycle', () => {
  let unsubscribe: (() => void) | null = null

  beforeEach(() => {
    markSessionSignedOut()
    unsubscribe = null
  })

  afterEach(() => {
    unsubscribe?.()
    markSessionSignedOut()
    client.interceptors.error.clear()
  })

  it('turns a 401 on a live session into one expiry', () => {
    const listener = vi.fn()
    unsubscribe = subscribeToSessionExpired(listener)
    markSessionSignedIn()

    reportUnauthorizedResponse(401)

    expect(listener).toHaveBeenCalledTimes(1)
  })

  it('collapses a burst of parallel 401s into a single expiry', () => {
    // The screen a session dies on has several requests in flight. Each one
    // gets its own 401; the user must not get one banner per request.
    const listener = vi.fn()
    unsubscribe = subscribeToSessionExpired(listener)
    markSessionSignedIn()

    for (let index = 0; index < 8; index += 1) reportUnauthorizedResponse(401)

    expect(listener).toHaveBeenCalledTimes(1)
  })

  it('stays silent for a 401 when nobody is signed in', () => {
    // A rejected sign-in attempt is an ordinary failed request, not an expiry.
    const listener = vi.fn()
    unsubscribe = subscribeToSessionExpired(listener)

    reportUnauthorizedResponse(401)

    expect(listener).not.toHaveBeenCalled()
  })

  it('stays silent for a 401 arriving after a deliberate sign-out', () => {
    const listener = vi.fn()
    unsubscribe = subscribeToSessionExpired(listener)
    markSessionSignedIn()
    markSessionSignedOut()

    reportUnauthorizedResponse(401)

    expect(listener).not.toHaveBeenCalled()
  })

  it('ignores statuses that do not mean the session ended', () => {
    const listener = vi.fn()
    unsubscribe = subscribeToSessionExpired(listener)
    markSessionSignedIn()

    for (const status of [0, 400, 403, 404, 429, 500, 503]) {
      reportUnauthorizedResponse(status)
    }

    expect(listener).not.toHaveBeenCalled()
  })

  it('re-arms after a new sign-in', () => {
    const listener = vi.fn()
    unsubscribe = subscribeToSessionExpired(listener)

    markSessionSignedIn()
    reportUnauthorizedResponse(401)
    markSessionSignedIn()
    reportUnauthorizedResponse(401)

    expect(listener).toHaveBeenCalledTimes(2)
  })

  it('expires the session from a generated-client 401 and returns the error untouched', async () => {
    const listener = vi.fn()
    unsubscribe = subscribeToSessionExpired(listener)
    installSessionExpiryInterceptor()
    markSessionSignedIn()

    const result = await client.get({
      // The app ships an empty baseUrl (same-origin); under Node the request
      // needs an absolute origin to parse at all.
      baseUrl: 'http://api.test',
      url: '/v1/anything',
      fetch: () =>
        Promise.resolve(
          new Response(JSON.stringify({ detail: 'unauthorized' }), {
            status: 401,
            headers: { 'Content-Type': 'application/json' },
          }),
        ),
    })

    expect(listener).toHaveBeenCalledTimes(1)
    expect(result.error).toEqual({ detail: 'unauthorized' })
  })

  it('does not expire the session on a transport failure with no response', async () => {
    const listener = vi.fn()
    unsubscribe = subscribeToSessionExpired(listener)
    installSessionExpiryInterceptor()
    markSessionSignedIn()

    await client.get({
      baseUrl: 'http://api.test',
      url: '/v1/anything',
      fetch: () => Promise.reject(new Error('network down')),
    })

    expect(listener).not.toHaveBeenCalled()
  })
})
