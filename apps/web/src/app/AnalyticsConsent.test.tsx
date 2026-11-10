import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'

import {
  beginAccountTelemetryConsent,
  clearAccountTelemetryConsent,
  failAccountTelemetryConsent,
  markTelemetryConsentPending,
  TELEMETRY_CONSENT_STORAGE_KEY,
} from '@/shared/lib/telemetry/consent'
import { AnalyticsConsent } from './AnalyticsConsent'

describe('analytics consent banner', () => {
  beforeEach(() => {
    clearAccountTelemetryConsent()
    localStorage.clear()
  })

  it('appears until the viewer makes a choice and records a grant', async () => {
    const user = userEvent.setup()
    render(<AnalyticsConsent />)

    expect(screen.getByTestId('analytics-consent')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Allow' }))

    expect(localStorage.getItem(TELEMETRY_CONSENT_STORAGE_KEY)).toBe('granted')
    expect(screen.queryByTestId('analytics-consent')).toBeNull()
  })

  it('records a denial and disappears', async () => {
    const user = userEvent.setup()
    render(<AnalyticsConsent />)

    await user.click(screen.getByRole('button', { name: 'Decline' }))

    expect(localStorage.getItem(TELEMETRY_CONSENT_STORAGE_KEY)).toBe('denied')
    expect(screen.queryByTestId('analytics-consent')).toBeNull()
  })

  it('does not render after an earlier decision', () => {
    localStorage.setItem(TELEMETRY_CONSENT_STORAGE_KEY, 'denied')
    render(<AnalyticsConsent />)
    expect(screen.queryByTestId('analytics-consent')).toBeNull()
  })

  it('stays hidden while the account preference is loading', () => {
    markTelemetryConsentPending()
    render(<AnalyticsConsent />)
    expect(screen.queryByTestId('analytics-consent')).toBeNull()
  })

  it('stays hidden when the account preference could not be loaded', () => {
    beginAccountTelemetryConsent('principal-a')
    failAccountTelemetryConsent('principal-a')
    render(<AnalyticsConsent />)
    expect(screen.queryByTestId('analytics-consent')).toBeNull()
  })
})
