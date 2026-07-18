import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { AppProvider } from '@/shared/contexts/AppContext'
import { useApp } from '@/shared/contexts/app-context'
import i18n from '@/shared/i18n'

const { authApiMock } = vi.hoisted(() => ({
  authApiMock: {
    resolveSession: vi.fn(),
    login: vi.fn(),
    logout: vi.fn(),
    bootstrapSetup: vi.fn(),
  },
}))

vi.mock('@/shared/api', () => ({
  authApi: authApiMock,
  ApiError: class MockApiError extends Error {
    status: number
    body: Record<string, unknown>

    constructor(status: number, body: Record<string, unknown>) {
      super(String(body?.error ?? body?.message ?? `API error ${status}`))
      this.status = status
      this.body = body
    }
  },
}))

function AppContextProbe() {
  const {
    workspaces,
    activeWorkspace,
    libraries,
    activeLibrary,
    locale,
    setActiveWorkspace,
    setLocale,
  } = useApp()

  return (
    <div>
      <div data-testid="locale">{locale}</div>
      <div data-testid="active-workspace">{activeWorkspace?.id ?? 'none'}</div>
      <div data-testid="active-library">{activeLibrary?.id ?? 'none'}</div>
      <div data-testid="visible-libraries">{libraries.map((library) => library.id).join(',')}</div>
      <div data-testid="library-readiness">
        {libraries
          .map(
            (library) =>
              `${library.id}:${library.queryReady ? 'ready' : 'blocked'}:${library.missingBindingPurposes.join('|')}`,
          )
          .join(',')}
      </div>
      <button type="button" onClick={() => setLocale('ru')}>
        Set Russian
      </button>
      {workspaces.map((workspace) => (
        <button key={workspace.id} onClick={() => setActiveWorkspace(workspace)} type="button">
          {workspace.name}
        </button>
      ))}
    </div>
  )
}

function makeSession() {
  return {
    mode: 'authenticated' as const,
    locale: 'en',
    session: { id: 'session-1', expiresAt: '2026-04-12T12:00:00Z' },
    me: {
      principal: { id: 'principal-1', displayLabel: 'Admin User' },
      user: { login: 'admin', displayName: 'Admin User' },
    },
    shellBootstrap: {
      workspaces: [
        { id: 'ws-default', slug: 'default', name: 'Default workspace' },
        { id: 'ws-qg', slug: 'qg', name: 'Quality Gates' },
      ],
      libraries: [
        {
          id: 'lib-default',
          workspaceId: 'ws-default',
          slug: 'default-library',
          name: 'Default library',
          ingestionReady: true,
          missingBindingPurposes: [],
        },
        {
          id: 'lib-qg-1',
          workspaceId: 'ws-qg',
          slug: 'qg-lib-1',
          name: 'QG Lib Test',
          ingestionReady: true,
          missingBindingPurposes: [],
        },
        {
          id: 'lib-qg-2',
          workspaceId: 'ws-qg',
          slug: 'qg-lib-2',
          name: 'Quality Gate 2026-04-12',
          ingestionReady: true,
          missingBindingPurposes: [],
        },
        {
          id: 'lib-router-missing',
          workspaceId: 'ws-qg',
          slug: 'router-missing',
          name: 'Router Missing',
          ingestionReady: true,
          missingBindingPurposes: ['embed_chunk', 'query_compile', 'query_answer', 'extract_text'],
        },
        {
          id: 'lib-embed-missing',
          workspaceId: 'ws-qg',
          slug: 'embed-missing',
          name: 'Embedding Missing',
          ingestionReady: true,
          missingBindingPurposes: ['embed_chunk'],
        },
        {
          id: 'lib-extract-text-missing',
          workspaceId: 'ws-qg',
          slug: 'extract-text-missing',
          name: 'Extract Text Missing',
          ingestionReady: true,
          missingBindingPurposes: ['extract_text'],
        },
        {
          id: 'lib-agent-missing',
          workspaceId: 'ws-qg',
          slug: 'agent-missing',
          name: 'Agent Missing',
          ingestionReady: true,
          missingBindingPurposes: ['agent'],
        },
      ],
    },
    bootstrapStatus: { setupRequired: false },
    message: null,
  }
}

describe('AppContext workspace-library scoping', () => {
  let container: HTMLDivElement
  let root: Root | null

  beforeEach(() => {
    vi.clearAllMocks()
    ;(
      globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }
    ).IS_REACT_ACT_ENVIRONMENT = true
    localStorage.clear()
    container = document.createElement('div')
    document.body.appendChild(container)
    root = null
  })

  afterEach(async () => {
    if (root) {
      await act(async () => {
        root?.unmount()
      })
    }
    container.remove()
  })

  async function flushUi() {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0))
    })
  }

  async function renderProbe() {
    await act(async () => {
      root = createRoot(container)
      root.render(
        <AppProvider>
          <AppContextProbe />
        </AppProvider>,
      )
    })

    await flushUi()
    await flushUi()
  }

  it('falls back to a supported locale when persisted and server values are unknown', async () => {
    localStorage.setItem('ironrag_locale', 'unsupported')
    authApiMock.resolveSession.mockResolvedValue({ ...makeSession(), locale: 'unknown' })

    await renderProbe()

    expect(container.querySelector('[data-testid="locale"]')?.textContent).toBe('en')
    expect(localStorage.getItem('ironrag_locale')).toBe('en')
  })

  it('persists the selected locale even if the i18n update fails', async () => {
    authApiMock.resolveSession.mockResolvedValue(makeSession())
    const originalChangeLanguage = i18n.changeLanguage.bind(i18n)
    const changeLanguage = vi
      .spyOn(i18n, 'changeLanguage')
      .mockImplementation((language, ...args) =>
        language === 'ru'
          ? Promise.reject(new Error('offline'))
          : originalChangeLanguage(language, ...args),
      )

    await renderProbe()

    const setRussianButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent === 'Set Russian',
    )
    await act(async () => {
      setRussianButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })

    expect(changeLanguage).toHaveBeenLastCalledWith('ru')
    expect(localStorage.getItem('ironrag_locale')).toBe('ru')
    expect(container.querySelector('[data-testid="locale"]')?.textContent).toBe('ru')
  })

  it('keeps the active library inside the selected workspace on session restore', async () => {
    localStorage.setItem('ironrag_active_workspace', 'ws-default')
    localStorage.setItem('ironrag_active_library', 'lib-qg-1')
    authApiMock.resolveSession.mockResolvedValue(makeSession())

    await renderProbe()

    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe(
      'ws-default',
    )
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe(
      'lib-default',
    )
    expect(container.querySelector('[data-testid="visible-libraries"]')?.textContent).toBe(
      'lib-default',
    )
  })

  it('selects the first library and filters visible libraries when workspace changes', async () => {
    localStorage.setItem('ironrag_active_workspace', 'ws-qg')
    localStorage.setItem('ironrag_active_library', 'lib-qg-1')
    authApiMock.resolveSession.mockResolvedValue(makeSession())

    await renderProbe()

    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe('ws-qg')
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe('lib-qg-1')
    expect(container.querySelector('[data-testid="visible-libraries"]')?.textContent).toBe(
      'lib-qg-1,lib-qg-2,lib-router-missing,lib-embed-missing,lib-extract-text-missing,lib-agent-missing',
    )

    const defaultWorkspaceButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent === 'Default workspace',
    )
    expect(defaultWorkspaceButton).toBeTruthy()

    await act(async () => {
      defaultWorkspaceButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    await flushUi()

    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe(
      'ws-default',
    )
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe(
      'lib-default',
    )
    expect(container.querySelector('[data-testid="visible-libraries"]')?.textContent).toBe(
      'lib-default',
    )
  })

  it('preserves missing binding purposes while deriving readiness from executable query bindings', async () => {
    localStorage.setItem('ironrag_active_workspace', 'ws-qg')
    authApiMock.resolveSession.mockResolvedValue(makeSession())

    await renderProbe()

    expect(container.querySelector('[data-testid="library-readiness"]')?.textContent).toContain(
      'lib-router-missing:blocked:embed_chunk|query_compile|query_answer|extract_text',
    )
    expect(container.querySelector('[data-testid="library-readiness"]')?.textContent).toContain(
      'lib-embed-missing:blocked:embed_chunk',
    )
    expect(container.querySelector('[data-testid="library-readiness"]')?.textContent).toContain(
      'lib-extract-text-missing:ready:extract_text',
    )
    expect(container.querySelector('[data-testid="library-readiness"]')?.textContent).toContain(
      'lib-agent-missing:blocked:agent',
    )
  })
})
