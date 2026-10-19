import { describe, expect, it } from 'vitest'

import { loginNoticeKind, ssoErrorFromSearch } from './loginNotice'

describe('loginNoticeKind', () => {
  it('says nothing when nothing went wrong', () => {
    expect(
      loginNoticeKind({
        isSessionExpired: false,
        queryAccessFailure: null,
        sessionError: null,
        ssoError: null,
      }),
    ).toBe('none')
  })

  it('ranks the verdict of a single sign-on the user just came back from above everything', () => {
    // The person clicked SSO, went to the identity provider and was sent back
    // here: that verdict is the newest fact and the one they are waiting for.
    expect(
      loginNoticeKind({
        isSessionExpired: true,
        queryAccessFailure: 'claimed',
        sessionError: new Error('boom'),
        ssoError: 'denied',
      }),
    ).toBe('ssoDenied')
    expect(
      loginNoticeKind({
        isSessionExpired: true,
        queryAccessFailure: null,
        sessionError: null,
        ssoError: 'failed',
      }),
    ).toBe('ssoFailed')
  })

  it('ranks an expired session above every request-level failure', () => {
    // The session running out is why the person is on this screen at all; the
    // bootstrap request that happened to fail on the way here is not.
    expect(
      loginNoticeKind({
        isSessionExpired: true,
        queryAccessFailure: 'claimed',
        sessionError: new Error('boom'),
        ssoError: null,
      }),
    ).toBe('expired')
  })

  it('distinguishes an already-claimed instance from a forbidden one', () => {
    expect(
      loginNoticeKind({
        isSessionExpired: false,
        queryAccessFailure: 'claimed',
        sessionError: null,
        ssoError: null,
      }),
    ).toBe('claimed')
    expect(
      loginNoticeKind({
        isSessionExpired: false,
        queryAccessFailure: 'forbidden',
        sessionError: null,
        ssoError: null,
      }),
    ).toBe('forbidden')
  })

  it('falls back to the session error only when nothing more specific applies', () => {
    expect(
      loginNoticeKind({
        isSessionExpired: false,
        queryAccessFailure: null,
        sessionError: new Error('offline'),
        ssoError: null,
      }),
    ).toBe('unavailable')
  })
})

describe('ssoErrorFromSearch', () => {
  it('reads only the two verdicts the callback can send', () => {
    expect(ssoErrorFromSearch('?ssoError=denied')).toBe('denied')
    expect(ssoErrorFromSearch('?returnTo=%2Fgraph&ssoError=failed')).toBe('failed')
  })

  it('treats anything else as no verdict', () => {
    // The parameter is attacker-writable: an unknown value must not reach the
    // screen as text, and a missing one means an ordinary visit.
    expect(ssoErrorFromSearch('')).toBeNull()
    expect(ssoErrorFromSearch('?ssoError=')).toBeNull()
    expect(ssoErrorFromSearch('?ssoError=%3Cscript%3E')).toBeNull()
    expect(ssoErrorFromSearch('?ssoError=Denied')).toBeNull()
  })
})
