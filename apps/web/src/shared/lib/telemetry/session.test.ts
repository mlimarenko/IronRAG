import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { telemetrySessionId } from './session'

const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

describe('telemetry session id', () => {
  beforeEach(() => {
    sessionStorage.clear()
  })

  afterEach(() => {
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
  })

  it('creates one UUID-shaped identifier per tab and reuses the stored one', () => {
    const first = telemetrySessionId()
    expect(first).toMatch(UUID_V4)
    expect(telemetrySessionId()).toBe(first)
    expect(sessionStorage.getItem('ironrag.v1.telemetry.session')).toBe(first)
  })

  it('never derives the identifier from Math.random', () => {
    const random = vi.spyOn(Math, 'random')
    telemetrySessionId()
    expect(random).not.toHaveBeenCalled()
  })

  it('keeps one fallback identifier when sessionStorage throws', () => {
    vi.stubGlobal('sessionStorage', {
      getItem: () => {
        throw new Error('storage blocked')
      },
      setItem: () => {
        throw new Error('storage blocked')
      },
    })
    const first = telemetrySessionId()
    expect(first).toMatch(UUID_V4)
    expect(telemetrySessionId()).toBe(first)
  })

  it('derives a UUID v4 from getRandomValues when randomUUID is unavailable', () => {
    vi.stubGlobal('crypto', {
      getRandomValues: (bytes: Uint8Array) => {
        bytes.set(Array.from({ length: 16 }, (_, index) => index))
        return bytes
      },
    })

    expect(telemetrySessionId()).toBe('00010203-0405-4607-8809-0a0b0c0d0e0f')
  })

  it('stays unique within the tab without any Web Crypto API', () => {
    vi.stubGlobal('crypto', {})

    const first = telemetrySessionId()
    sessionStorage.clear()
    const second = telemetrySessionId()

    expect(first).toMatch(/^[a-z0-9]+-[a-z0-9]+$/)
    expect(second).not.toBe(first)
  })
})
