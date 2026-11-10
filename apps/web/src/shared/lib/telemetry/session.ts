/**
 * Anonymous, tab-scoped telemetry session.
 *
 * The identifier exists to group events of one browsing session. It is random,
 * stored in sessionStorage, never derived from an account and never sent to the
 * product API.
 */
const TELEMETRY_SESSION_STORAGE_KEY = 'ironrag.v1.telemetry.session'

const randomId = (): string => {
  const cryptoApi = globalThis.crypto
  if (typeof cryptoApi?.randomUUID === 'function') return cryptoApi.randomUUID()
  if (typeof cryptoApi?.getRandomValues === 'function') {
    const bytes = cryptoApi.getRandomValues(new Uint8Array(16))
    return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')
  }
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`
}

let fallbackSessionId: string | null = null

export function telemetrySessionId(): string {
  try {
    const existing = sessionStorage.getItem(TELEMETRY_SESSION_STORAGE_KEY)
    if (existing) return existing
    const created = randomId()
    sessionStorage.setItem(TELEMETRY_SESSION_STORAGE_KEY, created)
    return created
  } catch {
    fallbackSessionId ??= randomId()
    return fallbackSessionId
  }
}
