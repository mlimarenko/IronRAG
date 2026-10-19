import { render, screen } from '@testing-library/react'
import { http, HttpResponse } from 'msw'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import type * as SharedApi from '@/shared/api'
import { client } from '@/shared/api/generated/client.gen'
import {
  iamSessionResolve,
  iamSessionResolveLibrary,
  opsLibraryDashboard,
} from '@/shared/api/mocks/fixtures'
import { DASHBOARD_POLL_INTERVAL_MS } from '@/features/dashboard/DashboardPage'
import { server } from '@/shared/api/mocks/server'
import App from './App'

// The whole point of this file is that nothing between the component and the
// wire is stubbed: the real `useQuery`, the real generated query options, the
// real generated client, and MSW answering an actual request. Only the session
// resolve is mocked, because it is the one call that runs before the query
// client exists and it is what decides when the authenticated tree mounts.
const { authApiMock } = vi.hoisted(() => ({
  authApiMock: {
    resolveSession: vi.fn(),
    login: vi.fn(),
    logout: vi.fn(),
    bootstrapSetup: vi.fn(),
    getIamBootstrap: vi.fn(),
  },
}))

vi.mock('@/shared/api', async (importOriginal) => ({
  ...(await importOriginal<typeof SharedApi>()),
  authApi: authApiMock,
}))

// The settle poll is the app's only recovery path for a stalled dashboard
// query. Every assertion here has to land before its first tick, or a screen
// that merely heals on that tick reads as healthy.
const BEFORE_FIRST_POLL_MS = DASHBOARD_POLL_INTERVAL_MS - 1_000

/// The landing page of a cold load mounts in the same commit that first names
/// the signed-in principal. Anything that reacts to that identity from an
/// effect around the routes therefore runs *after* the page has already started
/// fetching — and wiping the query cache there cancels that fetch silently:
/// React Query freezes the query at pending/fetching, tells no observer, and the
/// page keeps its skeleton with no request ever reaching the network.
describe('cold load with the dashboard as the landing route', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    localStorage.clear()
    // jsdom has no implicit base for a relative request URL the way a browser
    // does, so give the generated client the document origin MSW listens on.
    client.setConfig({ baseUrl: window.location.origin })
    window.history.pushState({}, '', '/dashboard')
  })

  afterEach(() => {
    client.setConfig({ baseUrl: '' })
    window.history.pushState({}, '', '/')
  })

  // One cold load, both queries that its landing commit mounts. They are
  // asserted together because the app keeps a single query client for the whole
  // module: a second render in the same file would reuse the first one's cache
  // and could no longer tell a dispatched request from a remembered answer.
  it('dispatches every query its landing commit mounts, without waiting for a poll', async () => {
    const dashboardRequests: string[] = []
    server.use(
      http.get('/v1/ops/libraries/:libraryId/dashboard', ({ params }) => {
        dashboardRequests.push(String(params.libraryId))
        return HttpResponse.json(opsLibraryDashboard())
      }),
      http.get('/v1/version/latest', () =>
        HttpResponse.json({
          checkedAt: '2026-08-16T00:00:00.000Z',
          currentVersion: '1.0.0',
          latestVersion: '9.9.9',
          releaseUrl: 'https://releases.example.test/9.9.9',
          repositoryUrl: 'https://repo.example.test',
          status: 'update_available',
        }),
      ),
    )
    authApiMock.resolveSession.mockResolvedValue(
      iamSessionResolve([iamSessionResolveLibrary('library-1')]),
    )

    render(<App />)

    // A document name only the payload carries: it is on screen exactly when
    // the request was really made, really answered, and really rendered.
    await screen.findByText('onboarding-guide.md', undefined, {
      timeout: BEFORE_FIRST_POLL_MS,
    })
    expect(document.querySelector('.dashboard-skeleton')).toBeNull()
    // Exactly one: not zero-then-healed-by-a-poll, and not a wasted first
    // attempt that was aborted before it left the client.
    expect(dashboardRequests).toEqual(['library-1'])

    // The shell's release check has no poll behind it at all. A silent cancel
    // leaves it pending forever even though its HTTP response arrived, so the
    // banner it feeds proves the whole commit's queries survived — not only the
    // one screen that happens to re-arm itself.
    await screen.findByText(/Update available/, undefined, { timeout: BEFORE_FIRST_POLL_MS })
  })
})
