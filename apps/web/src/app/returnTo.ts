export const DEFAULT_SIGNED_IN_ROUTE = '/dashboard'

/**
 * Where to send someone once they are signed in.
 *
 * The sign-in route carries the page the user was heading for in router state.
 * Only a same-document, absolute path is accepted: a protocol-relative value
 * (`//evil.example`) would leave the app entirely, and `/login` itself would
 * bounce straight back to this screen.
 */
export function returnToFromRouterState(state: unknown): string {
  if (typeof state !== 'object' || state === null) return DEFAULT_SIGNED_IN_ROUTE
  const from = (state as { from?: unknown }).from
  if (typeof from !== 'string') return DEFAULT_SIGNED_IN_ROUTE
  if (!from.startsWith('/') || from.startsWith('//')) return DEFAULT_SIGNED_IN_ROUTE
  if (from === '/login' || from.startsWith('/login?') || from.startsWith('/login#')) {
    return DEFAULT_SIGNED_IN_ROUTE
  }
  return from
}

/**
 * Where to send someone from the sign-in screen itself. The router state the
 * app set on the way here wins; failing that, the `returnTo` query the OIDC
 * callback carries back after a rejected single sign-on, so a retry still
 * lands where the person was going. Both pass the same same-document check.
 */
export function returnToFromLocation(location: Readonly<{ search: string; state: unknown }>) {
  const fromRouterState = returnToFromRouterState(location.state)
  if (fromRouterState !== DEFAULT_SIGNED_IN_ROUTE) return fromRouterState
  return returnToFromRouterState({ from: new URLSearchParams(location.search).get('returnTo') })
}
