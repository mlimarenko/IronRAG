import { act, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import type * as SharedApi from '@/shared/api'
import { iamSessionResolve } from '@/shared/api/mocks/fixtures'
import { reportUnauthorizedResponse } from '@/shared/api/sessionLifecycle'
import App from './App'

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

function guestSession() {
  return {
    mode: 'guest' as const,
    locale: 'en',
    session: null,
    me: null,
    shellBootstrap: null,
    bootstrapStatus: { setupRequired: false },
    message: null,
  }
}

const DEEP_LINK = '/nowhere?status=failed'

describe('App session expiry routing', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    localStorage.clear()
    window.history.pushState({}, '', DEEP_LINK)
  })

  afterEach(() => {
    window.history.pushState({}, '', '/')
  })

  it('sends an expired session to sign-in and back to the page it left', async () => {
    const user = userEvent.setup()
    authApiMock.resolveSession.mockResolvedValue(iamSessionResolve())
    render(<App />)
    await waitFor(() => {
      expect(window.location.pathname).toBe('/nowhere')
    })

    // Several in-flight requests discover the dead session at once.
    authApiMock.resolveSession.mockResolvedValue(guestSession())
    await act(async () => {
      for (let index = 0; index < 4; index += 1) reportUnauthorizedResponse(401)
    })

    await waitFor(() => {
      expect(window.location.pathname).toBe('/login')
    })
    // Exactly one message, and it says what to do rather than quoting the
    // first request that happened to fail.
    const alerts = await screen.findAllByRole('alert')
    expect(alerts).toHaveLength(1)
    expect(alerts[0]).toHaveTextContent('Your session has expired. Please sign in again.')

    authApiMock.login.mockResolvedValue({ id: 'session-2' })
    authApiMock.resolveSession.mockResolvedValue(iamSessionResolve())
    await user.type(screen.getByLabelText(/login/i), 'admin')
    await user.type(screen.getByLabelText(/password/i), 'correct horse')
    await user.click(screen.getByRole('button', { name: /sign in/i }))

    await waitFor(() => {
      expect(`${window.location.pathname}${window.location.search}`).toBe(DEEP_LINK)
    })
  })
})
