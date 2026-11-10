import { useEffect } from 'react'
import { useLocation } from 'react-router-dom'

import { trackEvent } from '@/shared/lib/telemetry/events'

/**
 * Emits one product event per route change. Telemetry itself decides whether
 * the viewer consented and whether the OTLP logger is active; the route is
 * normalised and attached by `trackEvent`.
 */
export function TelemetryRouteTracker() {
  const location = useLocation()

  useEffect(() => {
    trackEvent('page_view', { feature: 'navigation' })
  }, [location.pathname])

  return null
}
