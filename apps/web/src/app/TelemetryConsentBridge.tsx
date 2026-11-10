import { useEffect } from 'react'

import { useApp } from '@/shared/contexts/app-context'
import { syncAccountTelemetryConsent } from '@/shared/lib/telemetry/accountConsent'
import {
  clearAccountTelemetryConsent,
  markTelemetryConsentPending,
} from '@/shared/lib/telemetry/consent'
import { initObservability, watchTelemetryConsent } from '@/shared/lib/observability'

/**
 * The one bridge between the resolved account identity and telemetry consent.
 *
 * Nothing may start instrumentation until session resolution says whether a
 * principal is present. While an account preference loads, or when it cannot
 * be read, the consent store reports an unknown state and telemetry stays off.
 */
export function TelemetryConsentBridge() {
  const { user, isLoading } = useApp()
  const principalId = user?.id ?? null

  useEffect(() => {
    watchTelemetryConsent()
  }, [])

  useEffect(() => {
    if (isLoading) {
      markTelemetryConsentPending()
      return
    }
    if (!principalId) {
      clearAccountTelemetryConsent()
      void initObservability()
      return
    }
    void syncAccountTelemetryConsent(principalId)
  }, [isLoading, principalId])

  return null
}
