import { describe, expect, it } from 'vitest'

import routes from '../routes'

interface RouteRecord {
  path: string
  redirect?: string
  meta?: {
    shellSection?: string
  }
  children?: RouteRecord[]
}

function getShellChildren(): RouteRecord[] {
  const root = routes.at(0) as RouteRecord | undefined
  const shellHost = root?.children?.at(0)

  if (!shellHost?.children) {
    return []
  }

  return shellHost.children
}

describe('router two-page contract', () => {
  it('defaults the shell to Documents', () => {
    const shellChildren = getShellChildren()
    const defaultRoute = shellChildren.find((route) => route.path === '')

    expect(defaultRoute?.redirect).toBe('/documents')
  })

  it('keeps Documents and Ask as the only primary shell sections', () => {
    const shellChildren = getShellChildren()
    const primarySections = shellChildren
      .filter(
        (route) => route.meta?.shellSection === 'documents' || route.meta?.shellSection === 'ask',
      )
      .map((route) => ({ path: route.path, section: route.meta?.shellSection }))

    expect(primarySections).toEqual([
      { path: 'documents', section: 'documents' },
      { path: 'search', section: 'ask' },
    ])
  })

  it('preserves legacy aliases as safe redirects into the two-page contract', () => {
    const shellChildren = getShellChildren()
    const redirects = new Map(
      shellChildren
        .filter(
          (route): route is RouteRecord & { redirect: string } =>
            typeof route.redirect === 'string',
        )
        .map((route) => [route.path, route.redirect]),
    )

    expect(redirects.get('setup')).toBe('/documents')
    expect(redirects.get('processing')).toBe('/documents')
    expect(redirects.get('files')).toBe('/documents')
    expect(redirects.get('ingest')).toBe('/documents')
    expect(redirects.get('home')).toBe('/documents')
    expect(redirects.get('dashboard')).toBe('/documents')
    expect(redirects.get('ask')).toBe('/search')
    expect(redirects.get('chat')).toBe('/search')
    expect(redirects.get('graph')).toBe('/advanced/graph')
  })
})
