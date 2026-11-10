import { render } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

const appState = vi.hoisted(() => ({ user: null as { id: string } | null, isLoading: true }))
const clearAccount = vi.hoisted(() => vi.fn())
const markPending = vi.hoisted(() => vi.fn())
const syncAccount = vi.hoisted(() => vi.fn())
const watchConsent = vi.hoisted(() => vi.fn())
const initObservability = vi.hoisted(() => vi.fn())

vi.mock('@/shared/contexts/app-context', () => ({ useApp: () => appState }))
vi.mock('@/shared/lib/telemetry/consent', () => ({
  clearAccountTelemetryConsent: clearAccount,
  markTelemetryConsentPending: markPending,
}))
vi.mock('@/shared/lib/telemetry/accountConsent', () => ({
  syncAccountTelemetryConsent: syncAccount,
}))
vi.mock('@/shared/lib/observability', () => ({
  watchTelemetryConsent: watchConsent,
  initObservability,
}))

import { TelemetryConsentBridge } from './TelemetryConsentBridge'

describe('telemetry consent bridge', () => {
  beforeEach(() => {
    clearAccount.mockReset()
    markPending.mockReset()
    syncAccount.mockReset()
    watchConsent.mockReset()
    initObservability.mockReset()
  })

  it('does not use a browser choice while session resolution is pending', () => {
    appState.isLoading = true
    appState.user = null
    render(<TelemetryConsentBridge />)

    expect(watchConsent).toHaveBeenCalled()
    expect(markPending).toHaveBeenCalled()
    expect(clearAccount).not.toHaveBeenCalled()
    expect(syncAccount).not.toHaveBeenCalled()
  })

  it('enables the anonymous browser path after resolution', () => {
    appState.isLoading = false
    appState.user = null
    render(<TelemetryConsentBridge />)

    expect(clearAccount).toHaveBeenCalled()
    expect(syncAccount).not.toHaveBeenCalled()
    expect(initObservability).toHaveBeenCalled()
  })

  it('loads the account preference for the resolved principal', () => {
    appState.isLoading = false
    appState.user = { id: 'principal-a' }
    render(<TelemetryConsentBridge />)

    expect(syncAccount).toHaveBeenCalledWith('principal-a')
    expect(clearAccount).not.toHaveBeenCalled()
  })
})
