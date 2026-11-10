import { beforeEach, describe, expect, it, vi } from 'vitest'

import {
  requestTelemetryConsent,
  syncAccountTelemetryConsent,
  type TelemetryPreferencesApi,
} from './accountConsent'
import {
  applyAccountTelemetryConsent,
  beginAccountTelemetryConsent,
  clearAccountTelemetryConsent,
  readTelemetryConsentState,
  TELEMETRY_CONSENT_STORAGE_KEY,
} from './consent'

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((done) => {
    resolve = done
  })
  return { promise, resolve }
}

function fakeApi(overrides: Partial<TelemetryPreferencesApi> = {}): TelemetryPreferencesApi {
  return {
    load: vi.fn().mockResolvedValue({ telemetryConsent: null }),
    seed: vi
      .fn()
      .mockImplementation(async (_principal, consent) => ({ telemetryConsent: consent })),
    update: vi
      .fn()
      .mockImplementation(async (_principal, consent) => ({ telemetryConsent: consent })),
    ...overrides,
  }
}

describe('account telemetry consent', () => {
  beforeEach(() => {
    clearAccountTelemetryConsent()
    localStorage.clear()
  })

  it('lets an existing server denial win over a local grant', async () => {
    localStorage.setItem(TELEMETRY_CONSENT_STORAGE_KEY, 'granted')
    const api = fakeApi({ load: vi.fn().mockResolvedValue({ telemetryConsent: 'denied' }) })

    await syncAccountTelemetryConsent('principal-a', api)

    expect(api.seed).not.toHaveBeenCalled()
    expect(readTelemetryConsentState()).toMatchObject({
      principalId: 'principal-a',
      status: 'ready',
      consent: 'denied',
    })
    expect(localStorage.getItem(TELEMETRY_CONSENT_STORAGE_KEY)).toBeNull()
  })

  it('seeds an unset server preference from the local pre-sign-in choice', async () => {
    localStorage.setItem(TELEMETRY_CONSENT_STORAGE_KEY, 'granted')
    const api = fakeApi()

    await syncAccountTelemetryConsent('principal-a', api)

    expect(api.seed).toHaveBeenCalledWith('principal-a', 'granted')
    expect(readTelemetryConsentState()).toMatchObject({
      principalId: 'principal-a',
      status: 'ready',
      consent: 'granted',
    })
    expect(localStorage.getItem(TELEMETRY_CONSENT_STORAGE_KEY)).toBeNull()
  })

  it.each(['granted', 'denied'] as const)(
    'remembers a server %s across a later sign-in on another device',
    async (value) => {
      const api = fakeApi({ load: vi.fn().mockResolvedValue({ telemetryConsent: value }) })

      await syncAccountTelemetryConsent('principal-a', api)
      await syncAccountTelemetryConsent('principal-a', api)

      expect(readTelemetryConsentState()).toMatchObject({
        principalId: 'principal-a',
        status: 'ready',
        consent: value,
      })
      expect(api.seed).not.toHaveBeenCalled()
      expect(localStorage.getItem(TELEMETRY_CONSENT_STORAGE_KEY)).toBeNull()
    },
  )

  it('seeds an unset server preference from a local denial', async () => {
    localStorage.setItem(TELEMETRY_CONSENT_STORAGE_KEY, 'denied')
    const api = fakeApi()

    await syncAccountTelemetryConsent('principal-a', api)

    expect(api.seed).toHaveBeenCalledWith('principal-a', 'denied')
    expect(readTelemetryConsentState()).toMatchObject({ status: 'ready', consent: 'denied' })
  })

  it('leaves an unset account unset when there is no local choice', async () => {
    const api = fakeApi()

    await syncAccountTelemetryConsent('principal-a', api)

    expect(api.seed).not.toHaveBeenCalled()
    expect(readTelemetryConsentState()).toMatchObject({
      principalId: 'principal-a',
      status: 'ready',
      consent: 'unset',
    })
  })

  it('keeps telemetry off and preserves the local choice when loading fails', async () => {
    localStorage.setItem(TELEMETRY_CONSENT_STORAGE_KEY, 'denied')
    const api = fakeApi({ load: vi.fn().mockRejectedValue(new Error('offline')) })

    await syncAccountTelemetryConsent('principal-a', api)

    expect(readTelemetryConsentState()).toMatchObject({
      principalId: 'principal-a',
      status: 'error',
      consent: 'unset',
    })
    expect(localStorage.getItem(TELEMETRY_CONSENT_STORAGE_KEY)).toBe('denied')
  })

  it('keeps telemetry off and preserves the local choice when seeding fails', async () => {
    localStorage.setItem(TELEMETRY_CONSENT_STORAGE_KEY, 'granted')
    const api = fakeApi({ seed: vi.fn().mockRejectedValue(new Error('offline')) })

    await syncAccountTelemetryConsent('principal-a', api)

    expect(readTelemetryConsentState()).toMatchObject({ status: 'error', consent: 'unset' })
    expect(localStorage.getItem(TELEMETRY_CONSENT_STORAGE_KEY)).toBe('granted')
  })

  it('ignores a response for a principal that is no longer current', async () => {
    const first = deferred<{ telemetryConsent: 'granted' | 'denied' | null }>()
    const api = fakeApi({ load: vi.fn().mockReturnValue(first.promise) })

    const stale = syncAccountTelemetryConsent('principal-a', api)
    await syncAccountTelemetryConsent(
      'principal-b',
      fakeApi({ load: vi.fn().mockResolvedValue({ telemetryConsent: 'denied' }) }),
    )
    first.resolve({ telemetryConsent: 'granted' })
    await stale

    expect(readTelemetryConsentState()).toMatchObject({
      principalId: 'principal-b',
      status: 'ready',
      consent: 'denied',
    })
  })

  it('does not seed a new session using an earlier account response', async () => {
    localStorage.setItem(TELEMETRY_CONSENT_STORAGE_KEY, 'granted')
    const pending = deferred<{ telemetryConsent: 'granted' | 'denied' | null }>()
    const api = fakeApi({ load: vi.fn().mockReturnValue(pending.promise) })
    const stale = syncAccountTelemetryConsent('principal-a', api)
    await syncAccountTelemetryConsent('principal-b', fakeApi())
    pending.resolve({ telemetryConsent: null })
    await stale
    expect(api.seed).not.toHaveBeenCalled()
  })

  it('keeps a later denial effective while an earlier grant completes', async () => {
    beginAccountTelemetryConsent('principal-a')
    applyAccountTelemetryConsent('principal-a', 'denied')
    const pending = deferred<{ telemetryConsent: 'granted' | 'denied' | null }>()
    const api = fakeApi({
      update: vi
        .fn()
        .mockReturnValueOnce(pending.promise)
        .mockResolvedValue({ telemetryConsent: 'denied' }),
    })
    const grant = requestTelemetryConsent('granted', api)
    const denial = requestTelemetryConsent('denied', api)
    await Promise.resolve()
    expect(api.update).toHaveBeenCalledTimes(1)
    pending.resolve({ telemetryConsent: 'granted' })
    await Promise.all([grant, denial])
    expect(readTelemetryConsentState().consent).toBe('denied')
    expect(api.update).toHaveBeenNthCalledWith(1, 'principal-a', 'granted')
    expect(api.update).toHaveBeenNthCalledWith(2, 'principal-a', 'denied')
  })

  it('sends the original principal when a queued denial reaches a different cookie session', async () => {
    beginAccountTelemetryConsent('principal-a')
    applyAccountTelemetryConsent('principal-a', 'denied')
    let cookiePrincipal = 'principal-a'
    const stored = { 'principal-a': 'denied', 'principal-b': 'granted' }
    const pending = deferred<void>()
    const api = fakeApi({
      update: vi.fn(async (expectedPrincipal, choice) => {
        const principal = cookiePrincipal as keyof typeof stored
        if (expectedPrincipal !== principal) throw new Error('account session changed')
        if (choice === 'granted') await pending.promise
        stored[principal] = choice
        return { telemetryConsent: choice }
      }),
    })
    const grant = requestTelemetryConsent('granted', api)
    await Promise.resolve()
    const denial = requestTelemetryConsent('denied', api)
    cookiePrincipal = 'principal-b'
    pending.resolve()
    await Promise.all([grant, denial])
    expect(api.update).toHaveBeenNthCalledWith(2, 'principal-a', 'denied')
    expect(stored['principal-b']).toBe('granted')
    expect(stored['principal-a']).toBe('granted')
  })

  it('writes the browser choice before sign-in', async () => {
    await requestTelemetryConsent('granted', fakeApi())

    expect(localStorage.getItem(TELEMETRY_CONSENT_STORAGE_KEY)).toBe('granted')
    expect(readTelemetryConsentState()).toMatchObject({ status: 'anonymous', consent: 'granted' })
  })

  it('waits for the server before enabling an account choice', async () => {
    beginAccountTelemetryConsent('principal-a')
    applyAccountTelemetryConsent('principal-a', 'denied')
    const pending = deferred<{ telemetryConsent: 'granted' | 'denied' | null }>()
    const api = fakeApi({ update: vi.fn().mockReturnValue(pending.promise) })

    const update = requestTelemetryConsent('granted', api)
    expect(readTelemetryConsentState()).toMatchObject({ status: 'ready', consent: 'denied' })

    pending.resolve({ telemetryConsent: 'granted' })
    await update
    expect(readTelemetryConsentState()).toMatchObject({ status: 'ready', consent: 'granted' })
  })

  it('keeps the safe state when the server write fails', async () => {
    beginAccountTelemetryConsent('principal-a')
    applyAccountTelemetryConsent('principal-a', 'denied')
    const api = fakeApi({ update: vi.fn().mockRejectedValue(new Error('offline')) })

    await requestTelemetryConsent('granted', api)
    expect(readTelemetryConsentState()).toMatchObject({ status: 'ready', consent: 'denied' })

    beginAccountTelemetryConsent('principal-b')
    applyAccountTelemetryConsent('principal-b', 'granted')
    await requestTelemetryConsent('denied', api)
    expect(readTelemetryConsentState()).toMatchObject({ status: 'ready', consent: 'denied' })
  })

  it('disables an account choice immediately and still records the denial', async () => {
    beginAccountTelemetryConsent('principal-a')
    applyAccountTelemetryConsent('principal-a', 'granted')
    const pending = deferred<{ telemetryConsent: 'granted' | 'denied' | null }>()
    const api = fakeApi({ update: vi.fn().mockReturnValue(pending.promise) })

    const update = requestTelemetryConsent('denied', api)
    expect(readTelemetryConsentState()).toMatchObject({ status: 'ready', consent: 'denied' })

    pending.resolve({ telemetryConsent: 'denied' })
    await update
    expect(api.update).toHaveBeenCalledWith('principal-a', 'denied')
  })
})
