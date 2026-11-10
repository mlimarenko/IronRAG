import { normalizeRoute, sanitizeAttributes } from './redaction'
import { telemetrySessionId } from './session'

type ProductEventProperty = string | number | boolean | undefined | null
export type ProductEventProperties = Record<string, ProductEventProperty>
type ProductEventAttributes = Record<string, string | number | boolean>

export type ProductEventSink = (name: string, attributes: ProductEventAttributes) => void

const EVENT_NAME_PATTERN = /^[a-z][a-z0-9_]{1,63}$/

let sink: ProductEventSink | null = null

/** Wired once by observability after the OTLP logger exists. */
export function setProductEventSink(next: ProductEventSink | null): void {
  sink = next
}

const currentRoute = (): string => {
  if (typeof window === 'undefined') return 'unknown'
  return normalizeRoute(window.location.pathname)
}

/**
 * Record one product event. This is a no-op until consent and the OTLP logger
 * are both active, and it never throws into the UI that called it.
 */
export function trackEvent(name: string, properties: ProductEventProperties = {}): void {
  const activeSink = sink
  if (!activeSink || !EVENT_NAME_PATTERN.test(name)) return
  const attributes = sanitizeAttributes({
    ...properties,
    'event.name': name,
    'page.route': currentRoute(),
    'session.id': telemetrySessionId(),
  })
  try {
    activeSink(name, attributes)
  } catch {
    // Telemetry must never break a product interaction.
  }
}
