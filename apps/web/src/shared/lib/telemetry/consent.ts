/**
 * Browser and account telemetry consent.
 *
 * Before sign-in the choice belongs to this browser profile. Once a principal
 * is known, the account's server-side preference is authoritative and the
 * browser-local pre-sign-in value is consumed exactly once: it seeds an unset
 * account, never overrides an existing server choice, and cannot leak into the
 * next account that signs in on the same browser.
 */
export type TelemetryConsent = 'granted' | 'denied' | 'unset'
type TelemetryConsentStatus = 'anonymous' | 'loading' | 'ready' | 'error'

export interface TelemetryConsentState {
  consent: TelemetryConsent
  status: TelemetryConsentStatus
  principalId: string | null
}

export const TELEMETRY_CONSENT_STORAGE_KEY = 'ironrag.v1.telemetry.consent'
const TELEMETRY_CONSENT_OWNER_STORAGE_KEY = 'ironrag.v1.telemetry.consent.owner'
const TELEMETRY_CONSENT_CHANGED_EVENT = 'ironrag:telemetry-consent-changed'

type ExplicitConsent = Exclude<TelemetryConsent, 'unset'>

const parseConsent = (value: string | null): TelemetryConsent =>
  value === 'granted' || value === 'denied' ? value : 'unset'

function readBrowserConsent(): TelemetryConsent {
  try {
    return parseConsent(localStorage.getItem(TELEMETRY_CONSENT_STORAGE_KEY))
  } catch {
    // Storage may be blocked by the browser; without a recorded choice
    // telemetry stays off.
    return 'unset'
  }
}

function readBrowserOwner(): string | null {
  try {
    return localStorage.getItem(TELEMETRY_CONSENT_OWNER_STORAGE_KEY)
  } catch {
    return null
  }
}

function writeBrowserOwner(principal: string): void {
  try {
    localStorage.setItem(TELEMETRY_CONSENT_OWNER_STORAGE_KEY, principal)
  } catch {
    // Ownership is best-effort; an unscoped value is still not applied while
    // an account preference is authoritative.
  }
}

function removeBrowserChoice(): void {
  try {
    localStorage.removeItem(TELEMETRY_CONSENT_STORAGE_KEY)
    localStorage.removeItem(TELEMETRY_CONSENT_OWNER_STORAGE_KEY)
  } catch {
    // Storage may be blocked; the in-memory account state still supersedes it.
  }
}

let principalId: string | null = null
let status: TelemetryConsentStatus = 'anonymous'
let accountConsent: TelemetryConsent = 'unset'
let suppressBrowserConsent = false

function notify(): void {
  window.dispatchEvent(
    new CustomEvent<TelemetryConsent>(TELEMETRY_CONSENT_CHANGED_EVENT, {
      detail: currentConsent(),
    }),
  )
}

function currentConsent(): TelemetryConsent {
  if (status === 'anonymous') {
    return suppressBrowserConsent || readBrowserOwner() !== null ? 'unset' : readBrowserConsent()
  }
  return status === 'ready' ? accountConsent : 'unset'
}

export function readTelemetryConsentState(): TelemetryConsentState {
  return { consent: currentConsent(), status, principalId }
}

export function readTelemetryConsent(): TelemetryConsent {
  return currentConsent()
}

export function isTelemetryAllowed(): boolean {
  return currentConsent() === 'granted'
}

/** Persist the pre-sign-in browser choice and make it effective in this tab. */
export function writeBrowserTelemetryConsent(consent: ExplicitConsent): void {
  if (status !== 'anonymous') return
  try {
    localStorage.setItem(TELEMETRY_CONSENT_STORAGE_KEY, consent)
    localStorage.removeItem(TELEMETRY_CONSENT_OWNER_STORAGE_KEY)
  } catch {
    // The in-memory state below still updates this tab.
  }
  suppressBrowserConsent = false
  notify()
}

/**
 * Start resolving an account preference. The stored browser choice is kept
 * until the account preference has been settled, so a failed fetch can retry
 * without erasing the viewer's decision. It is claimed for this principal, so
 * a different account cannot use it as a seed. Anonymous state is suppressed
 * while the account is authoritative.
 */
export function beginAccountTelemetryConsent(nextPrincipalId: string): ExplicitConsent | null {
  const captured = readBrowserConsent()
  const owner = readBrowserOwner()
  let seedCandidate: ExplicitConsent | null = null
  if (captured !== 'unset' && (owner === null || owner === nextPrincipalId)) {
    seedCandidate = captured
    if (owner === null) writeBrowserOwner(nextPrincipalId)
  }
  suppressBrowserConsent = true
  principalId = nextPrincipalId
  status = 'loading'
  accountConsent = 'unset'
  notify()
  return seedCandidate
}

/**
 * The account preference is settled. Remove the browser choice only when it
 * belongs to this principal (or was unowned); a value claimed by another
 * principal is left for that principal's retry and is never applied here.
 */
export function completeAccountTelemetryConsent(nextPrincipalId: string): void {
  if (principalId !== nextPrincipalId || status !== 'loading') return
  const owner = readBrowserOwner()
  if (owner === null || owner === nextPrincipalId) {
    removeBrowserChoice()
    suppressBrowserConsent = false
  }
}

/** The shell has resolved to an unauthenticated visitor. */
export function clearAccountTelemetryConsent(): void {
  const changed = status !== 'anonymous' || principalId !== null
  const hadAccount = principalId !== null
  principalId = null
  status = 'anonymous'
  accountConsent = 'unset'
  if (hadAccount) {
    // A value kept for a failed account fetch belongs to that principal; do
    // not apply it to the next anonymous session or account.
    suppressBrowserConsent = readBrowserConsent() !== 'unset'
  }
  if (changed) notify()
}

/** Session resolution has not finished yet: no optional telemetry may emit. */
export function markTelemetryConsentPending(): void {
  if (status === 'loading' && principalId === null) return
  principalId = null
  status = 'loading'
  accountConsent = 'unset'
  notify()
}

export function applyAccountTelemetryConsent(
  nextPrincipalId: string,
  consent: TelemetryConsent,
): void {
  if (principalId !== nextPrincipalId || (status !== 'loading' && status !== 'ready')) return
  accountConsent = consent
  status = 'ready'
  notify()
}

export function failAccountTelemetryConsent(nextPrincipalId: string): void {
  if (principalId !== nextPrincipalId || status !== 'loading') return
  accountConsent = 'unset'
  status = 'error'
  notify()
}

export function subscribeTelemetryConsent(
  listener: (consent: TelemetryConsent) => void,
): () => void {
  const onCustom = () => listener(currentConsent())
  const onStorage = (event: StorageEvent) => {
    if (
      status === 'anonymous' &&
      (event.key === TELEMETRY_CONSENT_STORAGE_KEY ||
        event.key === TELEMETRY_CONSENT_OWNER_STORAGE_KEY)
    ) {
      listener(currentConsent())
    }
  }
  window.addEventListener(TELEMETRY_CONSENT_CHANGED_EVENT, onCustom)
  window.addEventListener('storage', onStorage)
  return () => {
    window.removeEventListener(TELEMETRY_CONSENT_CHANGED_EVENT, onCustom)
    window.removeEventListener('storage', onStorage)
  }
}
