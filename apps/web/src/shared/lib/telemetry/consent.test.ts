import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  applyAccountTelemetryConsent,
  beginAccountTelemetryConsent,
  clearAccountTelemetryConsent,
  completeAccountTelemetryConsent,
  failAccountTelemetryConsent,
  isTelemetryAllowed,
  markTelemetryConsentPending,
  readTelemetryConsent,
  readTelemetryConsentState,
  subscribeTelemetryConsent,
  TELEMETRY_CONSENT_STORAGE_KEY,
  writeBrowserTelemetryConsent,
} from './consent'

describe('telemetry consent', () => {
  afterEach(() => {
    clearAccountTelemetryConsent()
    localStorage.clear()
    vi.restoreAllMocks()
  })

  it('defaults to unset so telemetry stays off', () => {
    expect(readTelemetryConsent()).toBe('unset')
    expect(isTelemetryAllowed()).toBe(false)
  })

  it('persists an explicit pre-sign-in grant or denial', () => {
    writeBrowserTelemetryConsent('granted')
    expect(localStorage.getItem(TELEMETRY_CONSENT_STORAGE_KEY)).toBe('granted')
    expect(isTelemetryAllowed()).toBe(true)

    writeBrowserTelemetryConsent('denied')
    expect(readTelemetryConsent()).toBe('denied')
    expect(isTelemetryAllowed()).toBe(false)
  })

  it('notifies subscribers of changes in this tab and from another tab', () => {
    const listener = vi.fn()
    const unsubscribe = subscribeTelemetryConsent(listener)

    writeBrowserTelemetryConsent('granted')
    expect(listener).toHaveBeenCalledWith('granted')

    localStorage.setItem(TELEMETRY_CONSENT_STORAGE_KEY, 'denied')
    window.dispatchEvent(
      new StorageEvent('storage', { key: TELEMETRY_CONSENT_STORAGE_KEY, newValue: 'denied' }),
    )
    expect(listener).toHaveBeenLastCalledWith('denied')

    unsubscribe()
    writeBrowserTelemetryConsent('granted')
    expect(listener).toHaveBeenCalledTimes(2)
  })

  it('keeps telemetry off while an account preference is loading or failed', () => {
    localStorage.setItem(TELEMETRY_CONSENT_STORAGE_KEY, 'granted')
    expect(beginAccountTelemetryConsent('principal-a')).toBe('granted')
    expect(localStorage.getItem(TELEMETRY_CONSENT_STORAGE_KEY)).toBe('granted')
    expect(readTelemetryConsentState()).toMatchObject({
      principalId: 'principal-a',
      status: 'loading',
      consent: 'unset',
    })
    expect(isTelemetryAllowed()).toBe(false)

    failAccountTelemetryConsent('principal-a')
    expect(readTelemetryConsentState()).toMatchObject({ status: 'error', consent: 'unset' })
    expect(isTelemetryAllowed()).toBe(false)
    expect(localStorage.getItem(TELEMETRY_CONSENT_STORAGE_KEY)).toBe('granted')
  })

  it('applies the server choice and never inherits it after sign-out', () => {
    localStorage.setItem(TELEMETRY_CONSENT_STORAGE_KEY, 'granted')
    beginAccountTelemetryConsent('principal-a')
    completeAccountTelemetryConsent('principal-a')
    applyAccountTelemetryConsent('principal-a', 'denied')
    expect(readTelemetryConsentState()).toMatchObject({
      principalId: 'principal-a',
      status: 'ready',
      consent: 'denied',
    })
    expect(isTelemetryAllowed()).toBe(false)
    expect(localStorage.getItem(TELEMETRY_CONSENT_STORAGE_KEY)).toBeNull()

    clearAccountTelemetryConsent()
    expect(readTelemetryConsentState()).toMatchObject({
      principalId: null,
      status: 'anonymous',
      consent: 'unset',
    })
    expect(isTelemetryAllowed()).toBe(false)
  })

  it('does not leak a failed account seed into the next principal', () => {
    localStorage.setItem(TELEMETRY_CONSENT_STORAGE_KEY, 'granted')
    expect(beginAccountTelemetryConsent('principal-a')).toBe('granted')
    failAccountTelemetryConsent('principal-a')
    clearAccountTelemetryConsent()

    // Still anonymous: the stored decision stays for principal-a's retry, but
    // it is not applied to this anonymous session.
    expect(readTelemetryConsentState()).toMatchObject({ status: 'anonymous', consent: 'unset' })

    expect(beginAccountTelemetryConsent('principal-b')).toBeNull()
    expect(localStorage.getItem(TELEMETRY_CONSENT_STORAGE_KEY)).toBe('granted')
    applyAccountTelemetryConsent('principal-b', 'unset')
  })

  it('does not apply an account-owned browser choice after a page reload', async () => {
    localStorage.setItem(TELEMETRY_CONSENT_STORAGE_KEY, 'granted')
    beginAccountTelemetryConsent('principal-a')
    failAccountTelemetryConsent('principal-a')
    vi.resetModules()
    const reloaded = await import('./consent')
    expect(reloaded.readTelemetryConsent()).toBe('unset')
    expect(reloaded.isTelemetryAllowed()).toBe(false)
  })

  it('notifies another anonymous tab when its browser choice becomes account-owned', () => {
    writeBrowserTelemetryConsent('granted')
    const listener = vi.fn()
    const unsubscribe = subscribeTelemetryConsent(listener)
    localStorage.setItem('ironrag.v1.telemetry.consent.owner', 'principal-a')
    window.dispatchEvent(new StorageEvent('storage', { key: 'ironrag.v1.telemetry.consent.owner' }))
    expect(listener).toHaveBeenCalledWith('unset')
    unsubscribe()
  })

  it('lets the claimed principal complete the seed without erasing it early', () => {
    localStorage.setItem(TELEMETRY_CONSENT_STORAGE_KEY, 'denied')
    expect(beginAccountTelemetryConsent('principal-a')).toBe('denied')
    expect(localStorage.getItem(TELEMETRY_CONSENT_STORAGE_KEY)).toBe('denied')
    completeAccountTelemetryConsent('principal-a')
    expect(localStorage.getItem(TELEMETRY_CONSENT_STORAGE_KEY)).toBeNull()
    applyAccountTelemetryConsent('principal-a', 'denied')
    expect(readTelemetryConsentState()).toMatchObject({ status: 'ready', consent: 'denied' })
  })

  it('ignores an answer for a principal that is no longer current', () => {
    beginAccountTelemetryConsent('principal-a')
    beginAccountTelemetryConsent('principal-b')
    applyAccountTelemetryConsent('principal-a', 'granted')
    expect(readTelemetryConsentState()).toMatchObject({
      principalId: 'principal-b',
      status: 'loading',
      consent: 'unset',
    })
    applyAccountTelemetryConsent('principal-b', 'denied')
    expect(readTelemetryConsentState()).toMatchObject({ status: 'ready', consent: 'denied' })
  })

  it('marks unresolved session state as off instead of using a stale browser choice', () => {
    localStorage.setItem(TELEMETRY_CONSENT_STORAGE_KEY, 'granted')
    markTelemetryConsentPending()
    expect(readTelemetryConsentState()).toMatchObject({
      principalId: null,
      status: 'loading',
      consent: 'unset',
    })
    expect(isTelemetryAllowed()).toBe(false)
  })
})
