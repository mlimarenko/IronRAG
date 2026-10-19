import { describe, expect, it } from 'vitest'

import { errorMessage } from './errorMessage'

describe('errorMessage', () => {
  it('falls back for the RFC 9457 problem+json shape the API actually throws', () => {
    // apps/api/src/interfaces/http/router_support/problem_response.rs serializes
    // { type, title, status, detail, code }, never `message` — the generated
    // fetch client throws this object as-is on a non-2xx response. If this ever
    // picked up `.detail`, every "backend text leaking into a toast" guarantee
    // built on top of errorMessage() would silently break.
    const apiError = {
      code: 'not_found',
      detail: 'revision 11111111-1111-1111-1111-111111111111 not found',
      status: 404,
      title: 'Not Found',
      type: 'urn:ironrag:error:not_found',
    }

    expect(errorMessage(apiError, 'Failed to download this revision')).toBe(
      'Failed to download this revision',
    )
  })

  it('prefers an Error instance message over the fallback', () => {
    expect(errorMessage(new Error('boom'), 'fallback')).toBe('boom')
  })

  it('prefers a non-empty string thrown value over the fallback', () => {
    expect(errorMessage('network offline', 'fallback')).toBe('network offline')
  })

  it('falls back for an empty or whitespace-only string', () => {
    expect(errorMessage('   ', 'fallback')).toBe('fallback')
  })

  it('reads a plain object message field when present', () => {
    expect(errorMessage({ message: 'explicit message' }, 'fallback')).toBe('explicit message')
  })

  it('falls back for null, undefined, and non-string message fields', () => {
    expect(errorMessage(null, 'fallback')).toBe('fallback')
    expect(errorMessage(undefined, 'fallback')).toBe('fallback')
    expect(errorMessage({ message: 42 }, 'fallback')).toBe('fallback')
  })
})
