import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, useState } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { ApiError } from '@/shared/api'
import { reportUnauthorizedResponse } from '@/shared/api/sessionLifecycle'
import { AppProvider } from '@/shared/contexts/AppContext'
import { useApp } from '@/shared/contexts/app-context'
import i18n from '@/shared/i18n'

const { authApiMock, catalogMock } = vi.hoisted(() => ({
  authApiMock: {
    resolveSession: vi.fn(),
    login: vi.fn(),
    logout: vi.fn(),
    bootstrapSetup: vi.fn(),
  },
  catalogMock: {
    getCatalogLibrary: vi.fn(),
  },
}))

vi.mock('@/shared/api', () => ({
  authApi: authApiMock,
  Catalog: catalogMock,
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
    isAuthenticated,
    isLoading,
    isSessionExpired,
    sessionError,
    ssoAvailable,
    login,
    logout,
    selectLibrary,
    setActiveLibrary,
    setActiveWorkspace,
    setLocale,
  } = useApp()
  const [selectResult, setSelectResult] = useState('none')

  return (
    <div>
      <div data-testid="locale">{locale}</div>
      <div data-testid="select-result">{selectResult}</div>
      <div data-testid="active-workspace">{activeWorkspace?.id ?? 'none'}</div>
      <div data-testid="active-library">{activeLibrary?.id ?? 'none'}</div>
      <div data-testid="authenticated">{isAuthenticated ? 'yes' : 'no'}</div>
      <div data-testid="loading">{isLoading ? 'yes' : 'no'}</div>
      <div data-testid="session-error">{sessionError ?? 'none'}</div>
      <div data-testid="session-expired">{isSessionExpired ? 'yes' : 'no'}</div>
      <div data-testid="sso-available">{ssoAvailable ? 'yes' : 'no'}</div>
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
      <button type="button" onClick={() => setLocale('en')}>
        Set English
      </button>
      <button type="button" onClick={() => void login('member@example.test', 'correct horse')}>
        Log in
      </button>
      <button type="button" onClick={() => void logout()}>
        Log out
      </button>
      <button type="button" onClick={() => selectLibrary('lib-qg-2')}>
        Open QG library
      </button>
      <button type="button" onClick={() => selectLibrary('missing')}>
        Open missing library
      </button>
      <button
        type="button"
        onClick={() =>
          setSelectResult(selectLibrary('lib-outside-sample') ? 'selected' : 'rejected')
        }
      >
        Open out-of-sample library
      </button>
      {/* A link carries a library id and nothing more, and it may name one that
          lives in a workspace other than the one on screen. */}
      <button type="button" onClick={() => selectLibrary('lib-elsewhere')}>
        Open library from another workspace
      </button>
      {/* Admin screens hand over a fully loaded library from another workspace
          and nothing else — one writer for the whole transition. */}
      <button
        type="button"
        onClick={() =>
          setActiveLibrary({
            id: 'lib-qg-2',
            workspaceId: 'ws-qg',
            name: 'Quality Gate 2026-04-12',
            createdAt: '',
            includeDocumentHintInMcpAnswers: true,
            ingestionReady: true,
            queryReady: true,
            missingBindingPurposes: [],
          })
        }
      >
        Set library from another workspace
      </button>
      {workspaces.map((workspace) => (
        <button key={workspace.id} onClick={() => setActiveWorkspace(workspace)} type="button">
          {workspace.name}
        </button>
      ))}
      {/* The scope dialog switches the workspace and clears the library from a
          single click, so both updates run against the same render. */}
      {workspaces.map((workspace) => (
        <button
          key={`dialog-${workspace.id}`}
          onClick={() => {
            setActiveWorkspace(workspace)
            setActiveLibrary(null)
          }}
          type="button"
        >
          Dialog switch to {workspace.name}
        </button>
      ))}
    </div>
  )
}

// Emulates the server's shell bootstrap: the requested workspace wins when the
// caller may see it, otherwise the first one, and the active library is the
// requested one when it belongs to that workspace, otherwise the first there.
function makeSession(
  selection: {
    activeWorkspaceId?: string
    activeLibraryId?: string
    principalId?: string
  } = {},
) {
  const activeWorkspaceId = ['ws-default', 'ws-qg'].includes(selection.activeWorkspaceId ?? '')
    ? selection.activeWorkspaceId!
    : 'ws-default'
  const activeLibraryId =
    activeWorkspaceId === 'ws-qg'
      ? ([
          'lib-qg-1',
          'lib-qg-2',
          'lib-router-missing',
          'lib-embed-missing',
          'lib-extract-text-missing',
          'lib-agent-missing',
        ].find((id) => id === selection.activeLibraryId) ?? 'lib-qg-1')
      : 'lib-default'
  return {
    mode: 'authenticated' as const,
    locale: 'en',
    session: { id: 'session-1', expiresAt: '2026-04-12T12:00:00Z' },
    me: {
      principal: { id: selection.principalId ?? 'principal-1', displayLabel: 'Admin User' },
      user: { login: 'admin', displayName: 'Admin User' },
    },
    shellBootstrap: {
      activeWorkspaceId,
      activeLibraryId,
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
    bootstrapStatus: { setupRequired: false, ssoAvailable: false },
    message: null,
  }
}

// The server builds the library page from the workspace it considers active
// (apps/api/src/interfaces/shell.rs), so the shell never carries libraries of a
// workspace it is not currently on.
function activeWorkspaceSession(
  selection: { activeWorkspaceId?: string; activeLibraryId?: string } = {},
) {
  const session = makeSession(selection)
  session.shellBootstrap.libraries = session.shellBootstrap.libraries.filter(
    (library) => library.workspaceId === session.shellBootstrap.activeWorkspaceId,
  )
  return session
}

const PRINCIPAL = 'principal-1'

function storeScope(scope: { workspaceId?: string; libraryId?: string }) {
  localStorage.setItem(`ironrag.v1.scope.${PRINCIPAL}`, JSON.stringify(scope))
}

function storedScope(): { workspaceId?: string; libraryId?: string } {
  const raw = localStorage.getItem(`ironrag.v1.scope.${PRINCIPAL}`)
  return raw === null ? {} : (JSON.parse(raw) as { workspaceId?: string; libraryId?: string })
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

  // Held so a test can seed cached server state and then ask whether crossing
  // an identity boundary took it away.
  let queryClient: QueryClient

  async function flushUi() {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0))
    })
  }

  async function renderProbe() {
    queryClient = new QueryClient()
    await act(async () => {
      root = createRoot(container)
      root.render(
        // The provider owns the identity boundary over cached server state, so
        // it reads the query client the way the real app supplies it.
        <QueryClientProvider client={queryClient}>
          <AppProvider>
            <AppContextProbe />
          </AppProvider>
        </QueryClientProvider>,
      )
    })

    await flushUi()
    await flushUi()
  }

  // A workspace can hold thousands of libraries while the bootstrap carries a
  // bounded sample, so the remembered one is usually absent from it. Treating
  // "absent from the sample" as "gone" snapped the selection back to the first
  // library on every reload — and overwrote the stored id, so the choice could
  // never be recovered.
  function catalogLibrary(id: string, workspaceId: string) {
    return {
      data: {
        id,
        workspaceId,
        slug: `slug-${id}`,
        displayName: `Library ${id}`,
        includeDocumentHintInMcpAnswers: false,
        ingestionReadiness: { ready: true },
        runtimeReadiness: { missingBindingPurposes: [] },
      },
    }
  }

  it('keeps the chosen workspace when the library is cleared in the same handler', async () => {
    // The scope switcher set the workspace and then cleared the library in one
    // event handler. No render happens between the two calls, so the library
    // setter still saw the previous workspace in its closure and wrote it back
    // over the one just chosen — the switch snapped back within a second.
    authApiMock.resolveSession.mockImplementation((selection = {}) =>
      Promise.resolve(makeSession(selection)),
    )
    await renderProbe()
    await flushUi()
    expect(storedScope().workspaceId).toBe('ws-default')

    await act(async () => {
      const button = Array.from(container.querySelectorAll('button')).find(
        (element) => element.textContent === 'Dialog switch to Quality Gates',
      )
      button?.click()
    })
    await flushUi()

    expect(storedScope().workspaceId).toBe('ws-qg')
  })

  it('resolves a remembered library that the bootstrap sample does not carry', async () => {
    storeScope({ workspaceId: 'ws-qg', libraryId: 'lib-outside-sample' })
    authApiMock.resolveSession.mockResolvedValue(makeSession({ activeWorkspaceId: 'ws-qg' }))
    catalogMock.getCatalogLibrary.mockResolvedValue(catalogLibrary('lib-outside-sample', 'ws-qg'))

    await renderProbe()
    await flushUi()

    expect(catalogMock.getCatalogLibrary).toHaveBeenCalledWith({
      path: { libraryId: 'lib-outside-sample' },
    })
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe(
      'lib-outside-sample',
    )
    expect(storedScope().libraryId).toBe('lib-outside-sample')
  })

  it('keeps the remembered library id while the server has not answered yet', async () => {
    storeScope({ workspaceId: 'ws-qg', libraryId: 'lib-outside-sample' })
    authApiMock.resolveSession.mockResolvedValue(makeSession({ activeWorkspaceId: 'ws-qg' }))
    catalogMock.getCatalogLibrary.mockReturnValue(new Promise(() => undefined))

    await renderProbe()

    // The pending request must not cost the person their selection: a slow or
    // hanging lookup used to be indistinguishable from a deleted library.
    expect(storedScope().libraryId).toBe('lib-outside-sample')
  })

  it('keeps the remembered library when the lookup fails for transport reasons', async () => {
    storeScope({ workspaceId: 'ws-qg', libraryId: 'lib-outside-sample' })
    authApiMock.resolveSession.mockResolvedValue(makeSession({ activeWorkspaceId: 'ws-qg' }))
    catalogMock.getCatalogLibrary.mockRejectedValue(new ApiError(503, { error: 'unavailable' }))

    await renderProbe()
    await flushUi()

    // A flaky network is not evidence that the library disappeared.
    expect(storedScope().libraryId).toBe('lib-outside-sample')
  })

  it('synchronizes document language with persisted locale, switches and remount', async () => {
    const previousLanguage = document.documentElement.getAttribute('lang')
    document.documentElement.lang = 'en'
    localStorage.setItem('ironrag.v1.locale', 'ru')
    authApiMock.resolveSession.mockResolvedValue(makeSession())

    try {
      await renderProbe()
      expect(container.querySelector('[data-testid="locale"]')?.textContent).toBe('ru')
      expect(document.documentElement.lang).toBe('ru')

      for (const [language, buttonText] of [
        ['en', 'Set English'],
        ['ru', 'Set Russian'],
      ]) {
        const button = Array.from(container.querySelectorAll('button')).find(
          (element) => element.textContent === buttonText,
        )
        expect(button).toBeDefined()
        await act(async () => {
          button?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
        })
        expect(container.querySelector('[data-testid="locale"]')?.textContent).toBe(language)
        expect(document.documentElement.lang).toBe(language)
      }

      await act(async () => root?.unmount())
      root = null
      document.documentElement.lang = 'en'
      await renderProbe()
      expect(document.documentElement.lang).toBe('ru')
    } finally {
      if (previousLanguage === null) document.documentElement.removeAttribute('lang')
      else document.documentElement.lang = previousLanguage
    }
  })

  it('falls back to a supported locale when persisted and server values are unknown', async () => {
    localStorage.setItem('ironrag.v1.locale', 'unsupported')
    authApiMock.resolveSession.mockResolvedValue({ ...makeSession(), locale: 'unknown' })

    await renderProbe()

    expect(container.querySelector('[data-testid="locale"]')?.textContent).toBe('en')
    expect(localStorage.getItem('ironrag.v1.locale')).toBe('en')
  })

  it('surfaces SSO only when the server explicitly marks it available', async () => {
    const session = makeSession()
    authApiMock.resolveSession.mockResolvedValue({
      ...session,
      bootstrapStatus: { ...session.bootstrapStatus, ssoAvailable: true },
    })

    await renderProbe()

    expect(container.querySelector('[data-testid="sso-available"]')?.textContent).toBe('yes')
  })

  it('hides SSO when the server does not mark it available', async () => {
    authApiMock.resolveSession.mockResolvedValue(makeSession())

    await renderProbe()

    expect(container.querySelector('[data-testid="sso-available"]')?.textContent).toBe('no')
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
    expect(localStorage.getItem('ironrag.v1.locale')).toBe('ru')
    expect(container.querySelector('[data-testid="locale"]')?.textContent).toBe('ru')
  })

  it('ignores device-wide workspace and library storage keys', async () => {
    // Neither the pre-1.0 keys nor the device-wide ones may steer a selection:
    // a scope belongs to a principal, and only the principal-keyed entry counts.
    localStorage.setItem('ironrag_active_workspace', 'ws-qg')
    localStorage.setItem('ironrag_active_library', 'lib-qg-1')
    localStorage.setItem('ironrag.v1.active-workspace', 'ws-qg')
    localStorage.setItem('ironrag.v1.active-library', 'lib-qg-1')
    authApiMock.resolveSession.mockImplementation((selection = {}) =>
      Promise.resolve(makeSession(selection)),
    )

    await renderProbe()

    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe(
      'ws-default',
    )
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe(
      'lib-default',
    )
  })

  it('keeps the active library inside the selected workspace on session restore', async () => {
    storeScope({ workspaceId: 'ws-default', libraryId: 'lib-qg-1' })
    authApiMock.resolveSession.mockImplementation((selection = {}) =>
      Promise.resolve(makeSession(selection)),
    )

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
    storeScope({ workspaceId: 'ws-qg', libraryId: 'lib-qg-1' })
    authApiMock.resolveSession.mockImplementation((selection = {}) =>
      Promise.resolve(makeSession(selection)),
    )

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

  it('re-resolves the session when the target workspace has no locally known library', async () => {
    // The bootstrap only carries a bounded sample of the ACTIVE workspace's
    // libraries, so switching workspaces cannot be answered from loaded state.
    const scoped = makeSession({ activeWorkspaceId: 'ws-qg', activeLibraryId: 'lib-qg-1' })
    scoped.shellBootstrap.libraries = scoped.shellBootstrap.libraries.filter(
      (library) => library.workspaceId === 'ws-qg',
    )
    const switched = makeSession()
    switched.shellBootstrap.libraries = switched.shellBootstrap.libraries.filter(
      (library) => library.workspaceId === 'ws-default',
    )

    storeScope({ workspaceId: 'ws-qg', libraryId: 'lib-qg-1' })
    authApiMock.resolveSession.mockResolvedValueOnce(scoped).mockResolvedValueOnce(switched)

    await renderProbe()
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe('lib-qg-1')
    const callsBeforeSwitch = authApiMock.resolveSession.mock.calls.length

    const defaultWorkspaceButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent === 'Default workspace',
    )
    await act(async () => {
      defaultWorkspaceButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    await flushUi()

    expect(authApiMock.resolveSession.mock.calls.length).toBeGreaterThan(callsBeforeSwitch)
    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe(
      'ws-default',
    )
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe(
      'lib-default',
    )
  })

  it('lands on the chosen workspace when the same click also clears the library', async () => {
    // The scope dialog dispatches the workspace switch and a library reset from
    // one click, so both updates see the same render. A library update that
    // restates the workspace it captured at render time wrote the PREVIOUS
    // workspace back over the one just chosen; the pinned re-resolve was then
    // never sent, because the stored scope agreed with the server's hint-free
    // answer, and the shell snapped back to the workspace being left.
    authApiMock.resolveSession.mockImplementation((selection = {}) =>
      Promise.resolve(activeWorkspaceSession(selection)),
    )

    await renderProbe()
    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe(
      'ws-default',
    )

    const dialogSwitchButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent === 'Dialog switch to Quality Gates',
    )
    expect(dialogSwitchButton).toBeTruthy()
    await act(async () => {
      dialogSwitchButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    await flushUi()
    await flushUi()

    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe('ws-qg')
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe('lib-qg-1')
    expect(storedScope()).toEqual({ workspaceId: 'ws-qg', libraryId: 'lib-qg-1' })
    // The mechanism, not just the outcome: the target workspace is unknown to
    // the loaded state, so it can only be reached by asking for it by name.
    expect(
      authApiMock.resolveSession.mock.calls.some(
        ([selection]) => selection?.activeWorkspaceId === 'ws-qg',
      ),
    ).toBe(true)
  })

  it('opens a library the bounded sample does not carry', async () => {
    // "Absent from the sample" is not "does not exist": reporting the pair
    // invalid is how a deep link to a library outside the sample was ignored.
    authApiMock.resolveSession.mockImplementation((selection = {}) =>
      Promise.resolve(activeWorkspaceSession(selection)),
    )
    catalogMock.getCatalogLibrary.mockResolvedValue(catalogLibrary('lib-outside-sample', 'ws-qg'))

    await renderProbe()

    const openOutsideButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent === 'Open out-of-sample library',
    )
    await act(async () => {
      openOutsideButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    await flushUi()

    expect(container.querySelector('[data-testid="select-result"]')?.textContent).toBe('selected')
    expect(catalogMock.getCatalogLibrary).toHaveBeenCalledWith({
      path: { libraryId: 'lib-outside-sample' },
    })
    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe('ws-qg')
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe(
      'lib-outside-sample',
    )
    expect(storedScope()).toEqual({ workspaceId: 'ws-qg', libraryId: 'lib-outside-sample' })
  })

  it('moves to the workspace that owns a library named only by id', async () => {
    // A link carries a library id and nothing else, and the workspace it is
    // opened from is not necessarily the one that owns it. Landing the library
    // while leaving the workspace behind tears the scope: the shell then reads
    // one workspace's configuration under another workspace's name — which is
    // how an AI-config link opened on the wrong library's bindings.
    authApiMock.resolveSession.mockImplementation((selection = {}) =>
      Promise.resolve(activeWorkspaceSession(selection)),
    )
    catalogMock.getCatalogLibrary.mockResolvedValue(catalogLibrary('lib-elsewhere', 'ws-qg'))

    await renderProbe()
    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe(
      'ws-default',
    )

    const openElsewhereButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent === 'Open library from another workspace',
    )
    expect(openElsewhereButton).toBeTruthy()
    await act(async () => {
      openElsewhereButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    await flushUi()
    await flushUi()
    await flushUi()

    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe('ws-qg')
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe(
      'lib-elsewhere',
    )
    expect(storedScope()).toEqual({ workspaceId: 'ws-qg', libraryId: 'lib-elsewhere' })
    // The loaded sample belongs to the workspace being LEFT, so every library
    // picker stays empty unless the shell is asked again for the one entered.
    expect(container.querySelector('[data-testid="visible-libraries"]')?.textContent).toContain(
      'lib-qg-1',
    )
  })

  it('moves the workspace with the library when only the library is set', async () => {
    // Admin screens hand a fully loaded library from another workspace straight
    // to the setter. Because the setter did not move the workspace, each such
    // caller had to write it separately — two writers of one transition, the
    // shape that lost the switch in the scope dialog. One writer now, and it
    // costs exactly one session refresh: the sample of the workspace entered.
    authApiMock.resolveSession.mockImplementation((selection = {}) =>
      Promise.resolve(activeWorkspaceSession(selection)),
    )

    await renderProbe()
    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe(
      'ws-default',
    )
    const callsBefore = authApiMock.resolveSession.mock.calls.length

    const setElsewhereButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent === 'Set library from another workspace',
    )
    expect(setElsewhereButton).toBeTruthy()
    await act(async () => {
      setElsewhereButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    await flushUi()
    await flushUi()

    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe('ws-qg')
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe('lib-qg-2')
    expect(storedScope()).toEqual({ workspaceId: 'ws-qg', libraryId: 'lib-qg-2' })
    // A second writer would have refreshed the session on its own account; the
    // hint-free answer it applied is what this path must not pay for twice.
    expect(authApiMock.resolveSession.mock.calls.length - callsBefore).toBe(2)
  })

  it('surfaces a session refresh failure after switching to an unloaded workspace', async () => {
    const scoped = makeSession({ activeWorkspaceId: 'ws-qg', activeLibraryId: 'lib-qg-1' })
    scoped.shellBootstrap.libraries = scoped.shellBootstrap.libraries.filter(
      (library) => library.workspaceId === 'ws-qg',
    )

    storeScope({ workspaceId: 'ws-qg', libraryId: 'lib-qg-1' })
    authApiMock.resolveSession.mockResolvedValueOnce(scoped)

    await renderProbe()

    authApiMock.resolveSession.mockRejectedValue(new Error('Session refresh unavailable'))
    const defaultWorkspaceButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent === 'Default workspace',
    )
    await act(async () => {
      defaultWorkspaceButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    await flushUi()

    expect(container.querySelector('[data-testid="session-error"]')?.textContent).toBe(
      'Session refresh unavailable',
    )
  })

  it('preserves missing binding purposes while deriving readiness from executable query bindings', async () => {
    storeScope({ workspaceId: 'ws-qg' })
    authApiMock.resolveSession.mockImplementation((selection = {}) =>
      Promise.resolve(makeSession(selection)),
    )

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

  it('surfaces session resolution failures and completes loading', async () => {
    authApiMock.resolveSession.mockRejectedValue(new Error('Session service unavailable'))

    await renderProbe()

    expect(container.querySelector('[data-testid="loading"]')?.textContent).toBe('no')
    expect(container.querySelector('[data-testid="session-error"]')?.textContent).toBe(
      'Session service unavailable',
    )
    expect(container.querySelector('[data-testid="authenticated"]')?.textContent).toBe('no')
  })

  // The identity boundary exists to stop one principal's cached server state
  // from being read by the next one. Moving it out of a passive effect fixed a
  // wedged landing page, but nothing covered the property the boundary is FOR:
  // with the clear deleted outright the whole suite still passed, so the guard
  // could have been removed by accident and no test would have said a word.
  it('takes the departing principal cached server state with it', async () => {
    authApiMock.resolveSession
      .mockResolvedValueOnce(makeSession({ principalId: 'principal-1' }))
      .mockResolvedValueOnce(makeSession({ principalId: 'principal-2' }))
    authApiMock.login.mockResolvedValue({ id: 'session-2' })

    await renderProbe()
    queryClient.setQueryData(['documents', 'principal-1'], 'first principal payload')

    const logInButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent === 'Log in',
    )
    await act(async () => {
      logInButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
      await Promise.resolve()
    })
    await flushUi()

    expect(queryClient.getQueryData(['documents', 'principal-1'])).toBeUndefined()
  })

  // The other half, and the reason the boundary keys on the principal rather
  // than firing on every resolve: a re-resolve for the SAME person must not
  // throw away what the screens have already loaded.
  it('keeps cached server state when the same principal resolves again', async () => {
    authApiMock.resolveSession.mockResolvedValue(makeSession({ principalId: 'principal-1' }))
    authApiMock.login.mockResolvedValue({ id: 'session-2' })

    await renderProbe()
    queryClient.setQueryData(['documents', 'principal-1'], 'same principal payload')

    const logInButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent === 'Log in',
    )
    await act(async () => {
      logInButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
      await Promise.resolve()
    })
    await flushUi()

    expect(queryClient.getQueryData(['documents', 'principal-1'])).toBe('same principal payload')
  })

  it('restores a session after login and clears local state after a failed logout', async () => {
    authApiMock.resolveSession.mockImplementation((selection = {}) =>
      Promise.resolve(makeSession(selection)),
    )
    authApiMock.login.mockResolvedValue({ id: 'session-2' })
    authApiMock.logout.mockRejectedValue(new Error('network unavailable'))

    await renderProbe()

    const logInButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent === 'Log in',
    )
    await act(async () => {
      logInButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
      await Promise.resolve()
    })
    await flushUi()

    expect(authApiMock.login).toHaveBeenCalledWith('member@example.test', 'correct horse')
    expect(authApiMock.resolveSession).toHaveBeenCalledTimes(2)
    expect(container.querySelector('[data-testid="authenticated"]')?.textContent).toBe('yes')

    const logOutButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent === 'Log out',
    )
    await act(async () => {
      logOutButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
      await Promise.resolve()
    })

    expect(container.querySelector('[data-testid="authenticated"]')?.textContent).toBe('no')
    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe('none')
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe('none')
  })

  it('drops the signed-in identity once, however many requests report a dead session', async () => {
    authApiMock.resolveSession.mockImplementation((selection = {}) =>
      Promise.resolve(makeSession(selection)),
    )

    await renderProbe()
    expect(container.querySelector('[data-testid="authenticated"]')?.textContent).toBe('yes')

    await act(async () => {
      // Several in-flight requests come back 401 at once.
      for (let index = 0; index < 5; index += 1) reportUnauthorizedResponse(401)
    })

    expect(container.querySelector('[data-testid="authenticated"]')?.textContent).toBe('no')
    expect(container.querySelector('[data-testid="session-expired"]')?.textContent).toBe('yes')
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe('none')
  })

  it('does not raise an expiry banner for a 401 after the user signed out', async () => {
    authApiMock.resolveSession.mockImplementation((selection = {}) =>
      Promise.resolve(makeSession(selection)),
    )
    authApiMock.logout.mockResolvedValue(undefined)

    await renderProbe()
    const logOutButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent === 'Log out',
    )
    await act(async () => {
      logOutButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
      await Promise.resolve()
    })
    await flushUi()

    await act(async () => {
      reportUnauthorizedResponse(401)
    })

    expect(container.querySelector('[data-testid="session-expired"]')?.textContent).toBe('no')
  })

  it('does not hand one principal the library another principal selected', async () => {
    // The regression: the browser holds principal-1's scope and principal-2
    // signs in on the same machine. Nothing principal-1 chose may steer what
    // principal-2 sees, and no request may carry principal-1's library id.
    storeScope({ workspaceId: 'ws-qg', libraryId: 'lib-qg-2' })
    authApiMock.resolveSession.mockImplementation((selection = {}) =>
      Promise.resolve(makeSession({ ...selection, principalId: 'principal-2' })),
    )

    await renderProbe()

    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe(
      'ws-default',
    )
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe(
      'lib-default',
    )
    for (const [selection] of authApiMock.resolveSession.mock.calls) {
      expect(selection?.activeLibraryId).not.toBe('lib-qg-2')
    }
    // principal-1's own preference survives untouched, ready for their return.
    expect(storedScope()).toEqual({ workspaceId: 'ws-qg', libraryId: 'lib-qg-2' })
  })

  it('keeps each principal on their own remembered library', async () => {
    storeScope({ workspaceId: 'ws-qg', libraryId: 'lib-qg-2' })
    localStorage.setItem(
      'ironrag.v1.scope.principal-2',
      JSON.stringify({ workspaceId: 'ws-qg', libraryId: 'lib-agent-missing' }),
    )
    authApiMock.resolveSession.mockImplementation((selection = {}) =>
      Promise.resolve(makeSession({ ...selection, principalId: 'principal-2' })),
    )

    await renderProbe()

    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe(
      'lib-agent-missing',
    )
  })

  it('restores a library the bounded bootstrap sample does not carry', async () => {
    // A library picked through the full-catalog search can sit outside the
    // sample the shell ships. The first, hint-free resolve cannot name it, so
    // the app must ask again with it pinned instead of silently dropping it.
    storeScope({ workspaceId: 'ws-qg', libraryId: 'lib-agent-missing' })
    authApiMock.resolveSession.mockImplementation((selection = {}) => {
      const session = makeSession(selection)
      session.shellBootstrap.libraries = session.shellBootstrap.libraries.filter(
        (library) => library.workspaceId !== 'ws-qg' || library.id === selection.activeLibraryId,
      )
      return Promise.resolve(session)
    })

    await renderProbe()

    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe(
      'lib-agent-missing',
    )
  })

  it('falls back to an available library when the remembered one is gone', async () => {
    storeScope({ workspaceId: 'ws-qg', libraryId: 'lib-deleted' })
    authApiMock.resolveSession.mockImplementation((selection = {}) =>
      Promise.resolve(makeSession(selection)),
    )
    // "Gone" has to be established by the server. Absence from the bootstrap
    // sample says nothing: on a large workspace the sample holds almost none
    // of the libraries that do exist.
    catalogMock.getCatalogLibrary.mockRejectedValue(new ApiError(404, { error: 'not_found' }))

    await renderProbe()
    await flushUi()

    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe('ws-qg')
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe('lib-qg-1')
    expect(storedScope()).toEqual({ workspaceId: 'ws-qg', libraryId: 'lib-qg-1' })
  })

  it('selects a matching workspace and library only when both ids are valid', async () => {
    authApiMock.resolveSession.mockImplementation((selection = {}) =>
      Promise.resolve(makeSession(selection)),
    )

    await renderProbe()

    const openLibraryButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent === 'Open QG library',
    )
    await act(async () => {
      openLibraryButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })

    expect(container.querySelector('[data-testid="active-workspace"]')?.textContent).toBe('ws-qg')
    expect(container.querySelector('[data-testid="active-library"]')?.textContent).toBe('lib-qg-2')
    expect(storedScope()).toEqual({ workspaceId: 'ws-qg', libraryId: 'lib-qg-2' })
  })
})
