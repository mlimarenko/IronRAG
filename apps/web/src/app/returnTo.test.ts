import { describe, expect, it } from 'vitest'

import { DEFAULT_SIGNED_IN_ROUTE, returnToFromLocation, returnToFromRouterState } from './returnTo'

describe('returnToFromRouterState', () => {
  it('returns the page the user was heading for', () => {
    expect(returnToFromRouterState({ from: '/documents' })).toBe('/documents')
  })

  it('keeps the query and hash so a filtered view comes back filtered', () => {
    expect(returnToFromRouterState({ from: '/documents?status=failed#row-7' })).toBe(
      '/documents?status=failed#row-7',
    )
  })

  it('falls back when there is nothing to return to', () => {
    for (const state of [null, undefined, {}, { from: 42 }, 'documents']) {
      expect(returnToFromRouterState(state)).toBe(DEFAULT_SIGNED_IN_ROUTE)
    }
  })

  it('refuses to bounce sign-in back to sign-in', () => {
    for (const from of ['/login', '/login?next=/graph', '/login#form']) {
      expect(returnToFromRouterState({ from })).toBe(DEFAULT_SIGNED_IN_ROUTE)
    }
  })

  it('refuses destinations that would leave the application', () => {
    for (const from of [
      '//evil.example/phish',
      'https://evil.example/phish',
      'javascript:alert(1)',
    ]) {
      expect(returnToFromRouterState({ from })).toBe(DEFAULT_SIGNED_IN_ROUTE)
    }
  })
})

describe('returnToFromLocation', () => {
  it('prefers the page the app recorded on the way to sign-in', () => {
    expect(
      returnToFromLocation({ search: '?returnTo=%2Fgraph', state: { from: '/documents' } }),
    ).toBe('/documents')
  })

  it('falls back to the return route a rejected single sign-on carried back', () => {
    expect(
      returnToFromLocation({
        search: '?returnTo=%2Fgraph%3Flayout%3Dforce&ssoError=denied',
        state: null,
      }),
    ).toBe('/graph?layout=force')
  })

  it('holds the query to the same rules as router state', () => {
    for (const search of ['', '?returnTo=', '?returnTo=%2F%2Fevil.example', '?returnTo=%2Flogin']) {
      expect(returnToFromLocation({ search, state: null })).toBe(DEFAULT_SIGNED_IN_ROUTE)
    }
  })
})
