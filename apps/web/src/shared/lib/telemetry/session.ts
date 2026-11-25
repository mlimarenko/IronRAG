/**
 * Anonymous, tab-scoped telemetry session.
 *
 * The identifier exists to group events of one browsing session. It is random,
 * stored in sessionStorage, never derived from an account and never sent to the
 * product API.
 */
const TELEMETRY_SESSION_STORAGE_KEY = 'ironrag.v1.telemetry.session'

/** Format 16 random bytes as the UUID v4 the identifier has always looked like. */
const uuidFromBytes = (bytes: Uint8Array): string => {
  bytes[6] = (bytes[6]! & 0x0f) | 0x40
  bytes[8] = (bytes[8]! & 0x3f) | 0x80
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(
    16,
    20,
  )}-${hex.slice(20)}`
}

let fallbackCounter = 0

const randomId = (): string => {
  const cryptoApi = globalThis.crypto
  if (typeof cryptoApi?.randomUUID === 'function') return cryptoApi.randomUUID()
  if (typeof cryptoApi?.getRandomValues === 'function') {
    return uuidFromBytes(cryptoApi.getRandomValues(new Uint8Array(16)))
  }
  // No Web Crypto at all: stay unique within the tab without a seeded PRNG.
  fallbackCounter += 1
  return `${Date.now().toString(36)}-${fallbackCounter.toString(36)}`
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
