import { client } from './generated/client.gen'

// The one place the app learns that the server stopped accepting its session.
//
// Every transport funnels a 401 here — the generated client through an error
// interceptor, the hand-written streaming/download fetches through
// `reportUnauthorizedResponse`. The transition fires exactly once per signed-in
// session, so a page that had eight requests in flight when the session died
// produces one "session expired" outcome instead of eight.

type SessionExpiredListener = () => void

const listeners = new Set<SessionExpiredListener>()

// True only between a successful sign-in and the next expiry or sign-out. It
// is both the "is there something to lose" guard (a 401 on the login screen is
// an ordinary failed request, not an expiry) and the latch that collapses a
// burst of concurrent 401s into one transition.
let signedIn = false

/** Arm the expiry transition. Called once the app has a resolved identity. */
export function markSessionSignedIn(): void {
  signedIn = true
}

/**
 * Disarm the expiry transition for a session that ended on purpose (sign-out).
 * A later 401 from a request still in flight then stays silent.
 */
export function markSessionSignedOut(): void {
  signedIn = false
}

export function subscribeToSessionExpired(listener: SessionExpiredListener): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

/**
 * Funnel one HTTP status into the session lifecycle.
 *
 * Only a 401 on an armed session transitions, and the latch is flipped
 * synchronously before any listener runs, so concurrent callers cannot both
 * pass the guard.
 */
export function reportUnauthorizedResponse(status: number): void {
  if (status !== 401 || !signedIn) return
  signedIn = false
  // Snapshot first: a listener is free to unsubscribe (or subscribe) while the
  // notification loop is running, and the set must not change under it.
  const armed = Array.from(listeners)
  for (const listener of armed) listener()
}

/**
 * Route every generated-client failure through the lifecycle. The interceptor
 * runs before the error reaches any call site, so no screen has to know about
 * session expiry, and it re-throws nothing — the original error still reaches
 * the caller, which is by then being unmounted by the redirect to sign-in.
 */
export function installSessionExpiryInterceptor(): void {
  client.interceptors.error.use((error, response) => {
    // `response` is undefined for transport failures (offline, DNS, abort);
    // only a real HTTP status can mean the session ended.
    reportUnauthorizedResponse(response?.status ?? 0)
    return error
  })
}
