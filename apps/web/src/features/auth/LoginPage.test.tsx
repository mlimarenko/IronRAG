import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import type { ReactNode } from 'react'
import { MemoryRouter, useLocation, type InitialEntry } from 'react-router-dom'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { ApiError, authApi } from '@/shared/api'
import type { BootstrapStatus } from '@/shared/api/auth'

import LoginPage from './LoginPage'

const mocks = vi.hoisted(() => ({
  app: {
    bootstrapSetup: vi.fn(),
    isBootstrapRequired: false,
    isLoading: false,
    login: vi.fn(),
    sessionError: null as string | null,
    ssoAvailable: false,
    user: null,
  },
  preferences: {
    locale: 'en',
    setLocale: vi.fn(),
  },
}))

vi.mock('@/shared/contexts/app-context', () => ({
  useApp: () => mocks.app,
}))

vi.mock('@/app/preferences-context', () => ({
  usePreferences: () => mocks.preferences,
}))

const readyBootstrapStatus: BootstrapStatus = {
  setupRequired: true,
  ssoAvailable: false,
  aiSetup: {
    bindingBundles: [
      {
        apiKeyRequired: false,
        baseUrlPolicy: {
          allowOverride: false,
          allowPrivateNetwork: false,
          requireHttps: true,
          trimSuffixes: [],
        },
        baseUrlRequired: false,
        bindings: [
          {
            bindingPurpose: 'query_answer',
            modelCatalogId: 'model-answer',
            modelName: 'answer-model',
          },
        ],
        capabilities: {
          chat: 'supported',
          embeddings: 'unsupported',
          modelDiscovery: 'supported',
          streaming: 'supported',
          tools: 'supported',
          vision: 'unsupported',
        },
        credentialPolicy: {
          apiKeyRequired: false,
          baseUrlMode: 'fixed',
          baseUrlRequired: false,
          validationMode: 'model_list',
        },
        credentialSource: 'env',
        defaultBaseUrl: 'https://provider.example/v1',
        displayName: 'Provider Alpha',
        modelDiscovery: { mode: 'shared', paths: [] },
        providerCatalogId: 'provider-alpha-bootstrap',
        providerKind: 'provider_alpha',
        runtime: {
          authScheme: 'bearer',
          chatPath: '/chat/completions',
          kind: 'openai_compatible',
          modelsPath: '/models',
          structuredOutput: 'json_object',
          tokenLimitParameter: 'max_tokens',
        },
        uiHints: {},
      },
    ],
  },
}

function LocationProbe() {
  const location = useLocation()
  return <span data-testid="location">{`${location.pathname}${location.search}`}</span>
}

function renderLoginPage(initialEntries: InitialEntry[] = ['/login']) {
  const queryClient = new QueryClient({
    defaultOptions: {
      mutations: { retry: false },
      queries: { retry: false },
    },
  })

  function Wrapper({ children }: Readonly<{ children: ReactNode }>) {
    return (
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={initialEntries}>
          {children}
          <LocationProbe />
        </MemoryRouter>
      </QueryClientProvider>
    )
  }

  return render(<LoginPage />, { wrapper: Wrapper })
}

describe('LoginPage', () => {
  beforeEach(() => {
    mocks.app.bootstrapSetup.mockReset()
    mocks.app.isBootstrapRequired = false
    mocks.app.isLoading = false
    mocks.app.login.mockReset()
    mocks.app.sessionError = null
    mocks.app.ssoAvailable = false
    mocks.app.user = null
    mocks.preferences.locale = 'en'
    mocks.preferences.setLocale.mockReset()
    vi.spyOn(authApi, 'getIamBootstrap').mockResolvedValue(readyBootstrapStatus)
  })

  it('renders the guest login form', () => {
    renderLoginPage()

    const loginField = screen.getByLabelText('Login')
    const passwordField = screen.getByLabelText('Password')
    const loginForm = loginField.closest('form')

    expect(screen.getByRole('heading', { name: /sign in/i, level: 1 })).toBeInTheDocument()
    expect(screen.getAllByRole('heading')).toHaveLength(1)
    expect(document.querySelector('.login-story')).toBeNull()
    expect(loginField).toHaveAttribute('autocomplete', 'username')
    expect(loginField).toHaveAttribute('placeholder')
    expect(loginForm).toContainElement(screen.getByRole('button', { name: /language/i }))
    expect(passwordField).toHaveAttribute('autocomplete', 'current-password')
    expect(passwordField).toHaveAttribute('placeholder')
    expect(
      screen.queryByRole('link', { name: 'Continue with single sign-on' }),
    ).not.toBeInTheDocument()
    // No "remember me": session lifetime is a deployment policy (a sliding
    // inactivity window under an absolute cap), not a per-sign-in choice, so
    // the form must not offer a control that changes nothing.
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument()
  })

  it('offers configured SSO through the same-origin API and preserves a safe return route', () => {
    mocks.app.ssoAvailable = true
    renderLoginPage([
      {
        pathname: '/login',
        state: { from: '/graph?layout=force#selected-node' },
      },
    ])

    expect(screen.getByRole('link', { name: 'Continue with single sign-on' })).toHaveAttribute(
      'href',
      '/v1/iam/oidc/login?returnTo=%2Fgraph%3Flayout%3Dforce',
    )
  })

  it('does not carry an untrusted router destination into the SSO request', () => {
    mocks.app.ssoAvailable = true
    renderLoginPage([
      {
        pathname: '/login',
        state: { from: 'https://attacker.example/claim-session' },
      },
    ])

    expect(screen.getByRole('link', { name: 'Continue with single sign-on' })).toHaveAttribute(
      'href',
      '/v1/iam/oidc/login?returnTo=%2Fdashboard',
    )
  })

  it('explains a denied single sign-on and keeps the return route for a retry', async () => {
    mocks.app.ssoAvailable = true
    renderLoginPage(['/login?returnTo=%2Fgraph%3Flayout%3Dforce&ssoError=denied'])

    expect(screen.getByRole('alert')).toHaveTextContent(
      'Single sign-on access has not been granted. Contact your administrator.',
    )
    expect(screen.getByRole('link', { name: 'Continue with single sign-on' })).toHaveAttribute(
      'href',
      '/v1/iam/oidc/login?returnTo=%2Fgraph%3Flayout%3Dforce',
    )
    // The verdict is consumed from the address so a reload or a later
    // password sign-in does not keep repeating it; the return route stays.
    await waitFor(() => {
      expect(screen.getByTestId('location')).toHaveTextContent(
        '/login?returnTo=%2Fgraph%3Flayout%3Dforce',
      )
    })
    expect(screen.getByTestId('location')).not.toHaveTextContent('ssoError')
    expect(screen.getByRole('alert')).toBeInTheDocument()
  })

  it('offers a retry after a single sign-on that did not complete', async () => {
    mocks.app.ssoAvailable = true
    renderLoginPage(['/login?ssoError=failed'])

    expect(screen.getByRole('alert')).toHaveTextContent(
      'Single sign-on did not complete. Please try again.',
    )
    expect(screen.getByRole('link', { name: 'Continue with single sign-on' })).toHaveAttribute(
      'href',
      '/v1/iam/oidc/login?returnTo=%2Fdashboard',
    )
    await waitFor(() => {
      expect(screen.getByTestId('location')).toHaveTextContent(/^\/login$/)
    })
  })

  it('ignores an ssoError value it does not know', () => {
    renderLoginPage(['/login?ssoError=%3Cscript%3E'])

    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
  })

  it('shows an inline error when login returns 401', async () => {
    const user = userEvent.setup()
    mocks.app.login.mockRejectedValue(
      new ApiError(401, { code: 'invalid_credentials', detail: 'internal detail' }),
    )
    renderLoginPage()

    await user.clear(screen.getByLabelText('Login'))
    await user.type(screen.getByLabelText('Login'), 'operator')
    await user.clear(screen.getByLabelText('Password'))
    await user.type(screen.getByLabelText('Password'), 'correct horse')
    await user.click(screen.getByRole('button', { name: 'Sign in' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('Login or password is incorrect.')
    expect(mocks.app.login).toHaveBeenCalledWith('operator', 'correct horse')
  })

  it('renders the bootstrap wizard when setup is required', async () => {
    mocks.app.isBootstrapRequired = true
    renderLoginPage()

    expect(
      await screen.findByRole('heading', { name: 'Initial Setup', level: 1 }),
    ).toBeInTheDocument()
    expect(screen.getAllByRole('heading')).toHaveLength(1)
    expect(await screen.findByRole('button', { name: /Provider Alpha/ })).toBeInTheDocument()
    expect(authApi.getIamBootstrap).toHaveBeenCalledTimes(1)
  })

  it('keeps bootstrap submission disabled while owner data is invalid', async () => {
    mocks.app.isBootstrapRequired = true
    renderLoginPage()

    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Complete Setup' })).toBeDisabled()
    })
    expect(mocks.app.bootstrapSetup).not.toHaveBeenCalled()
  })

  it('returns to sign in with an explanation when bootstrap status is forbidden', async () => {
    mocks.app.isBootstrapRequired = true
    vi.mocked(authApi.getIamBootstrap).mockRejectedValue(
      new ApiError(403, { code: 'forbidden', detail: 'internal policy detail' }),
    )
    renderLoginPage()

    expect(await screen.findByRole('heading', { name: 'Sign in' })).toBeInTheDocument()
    expect(screen.getByRole('alert')).toHaveTextContent(
      'Initial setup is no longer available from this session.',
    )
    expect(screen.queryByText('internal policy detail')).not.toBeInTheDocument()
  })

  it('returns to sign in when another session claims bootstrap first', async () => {
    const user = userEvent.setup()
    mocks.app.isBootstrapRequired = true
    mocks.app.bootstrapSetup.mockRejectedValue(
      new ApiError(409, {
        code: 'bootstrap_already_claimed',
        detail: 'internal conflict detail',
      }),
    )
    renderLoginPage()

    await user.type(await screen.findByLabelText('Admin login'), 'owner')
    await user.type(screen.getByLabelText('Email'), 'owner@example.com')
    await user.type(screen.getByLabelText('Password'), 'secure-password')
    await user.click(screen.getByRole('button', { name: 'Complete Setup' }))

    expect(await screen.findByRole('heading', { name: 'Sign in' })).toBeInTheDocument()
    expect(screen.getByRole('alert')).toHaveTextContent(
      'Setup was completed in another session. Sign in to continue.',
    )
    expect(mocks.app.bootstrapSetup).toHaveBeenCalledWith({
      email: 'owner@example.com',
      login: 'owner',
      password: 'secure-password',
    })
  })
})
