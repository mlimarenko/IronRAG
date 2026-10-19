/**
 * The verdict the OIDC callback sends the browser back with. Exactly two
 * values exist on purpose: `denied` is an identity the provider vouched for
 * but this deployment does not admit, `failed` is everything else. Nothing
 * about the provider, the group, or the person travels in the address.
 */
export type SsoError = 'denied' | 'failed'

export function ssoErrorFromSearch(search: string): SsoError | null {
  const value = new URLSearchParams(search).get('ssoError')
  return value === 'denied' || value === 'failed' ? value : null
}

/**
 * Which single sentence the login form puts above the fields. The ranking is
 * the point: a single sign-on the person just came back from is the newest
 * fact and the one they are waiting on; after that an expired session
 * explains why they are looking at this screen at all, and it outranks
 * whichever individual request happened to fail on the way here.
 */
export function loginNoticeKind(
  input: Readonly<{
    isSessionExpired: boolean
    queryAccessFailure: 'forbidden' | 'claimed' | null
    sessionError: unknown
    ssoError: SsoError | null
  }>,
): 'ssoDenied' | 'ssoFailed' | 'expired' | 'claimed' | 'forbidden' | 'unavailable' | 'none' {
  if (input.ssoError === 'denied') return 'ssoDenied'
  if (input.ssoError === 'failed') return 'ssoFailed'
  if (input.isSessionExpired) return 'expired'
  if (input.queryAccessFailure === 'claimed') return 'claimed'
  if (input.queryAccessFailure === 'forbidden') return 'forbidden'
  return input.sessionError ? 'unavailable' : 'none'
}
